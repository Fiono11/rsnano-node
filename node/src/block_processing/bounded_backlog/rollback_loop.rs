use std::{
    sync::{Arc, MutexGuard},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;

use super::backlog_logic::BoundedBacklogLogic;

/// Continuously rolls back unconfirmed blocks with the lowest priority
/// if the backlog exceeds the configured limit
pub(crate) struct RollbackLoop {
    pub(super) state: Arc<NullableCondvarMutex<BoundedBacklogLogic>>,
    pub(super) ledger: Arc<Ledger>,
    pub(super) can_roll_back: Box<dyn Fn(&BlockHash) -> bool + Send + Sync>,
}

impl RollbackLoop {
    pub(crate) fn run(&self) {
        let mut state = self.state.lock();
        let mut targets = Vec::with_capacity(state.config.rollback_batch_size);

        while !state.stopped() {
            state.set_current_backlog_size(self.ledger.backlog_size());
            state = self
                .state
                .wait_timeout_while(state, Duration::from_secs(1), |i| {
                    !i.stopped() && !i.rollback_needed()
                })
                .0;

            state = self.run_one(state, &mut targets);
        }
    }

    fn run_one<'a>(
        &'a self,
        mut state: MutexGuard<'a, BoundedBacklogLogic>,
        targets: &mut Vec<BlockHash>,
    ) -> MutexGuard<'a, BoundedBacklogLogic> {
        if state.stopped() || !state.rollback_needed() {
            return state;
        }

        state.gather_targets(targets);

        if !targets.is_empty() {
            let target_count = state.rollback_target_count();
            drop(state);

            self.ledger
                .roll_back_batch(&*targets, target_count as usize, &self.can_roll_back);

            state = self.state.lock();
        }

        state
    }
}
