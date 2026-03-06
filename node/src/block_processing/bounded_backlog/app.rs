use std::{
    sync::{Arc, MutexGuard},
    time::Duration,
};

use tracing::info;

use rsnano_ledger::{Ledger, LedgerEvent};
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use super::{logic::BoundedBacklogLogic, walker::AccountWalker};
use crate::block_processing::{
    LedgerPipelineEvent, backlog_scan::UnconfirmedInfo, bounded_backlog::BoundedBacklogConfig,
};

/// Continuously rolls back unconfirmed blocks with the lowest priority
/// if the backlog exceeds the configured limit
/// This struct belongs to the application layer
pub struct BoundedBacklog {
    logic: NullableCondvarMutex<BoundedBacklogLogic>,
    ledger: Arc<Ledger>,
}

impl BoundedBacklog {
    pub fn new(config: BoundedBacklogConfig, ledger: Arc<Ledger>) -> Self {
        let logic = NullableCondvarMutex::new(BoundedBacklogLogic::new(config));
        Self { logic, ledger }
    }

    pub fn new_null() -> Self {
        Self::new(Default::default(), Ledger::new_null().into())
    }

    pub fn set_cooldown(&self, cool_down: bool) {
        self.logic.lock().set_cool_down(cool_down);
        self.logic.notify_all();
    }

    pub fn stop(&self) {
        self.logic.lock().stop();
        self.logic.notify_all();
    }

    pub(crate) fn run_loop(&self) {
        let mut logic = self.logic.lock();
        info!(
            "Bounded backlog enabled: max backlog={}, batch_size={}",
            logic.max_backlog(),
            logic.rollback_batch_size(),
        );

        let mut targets = Vec::with_capacity(logic.rollback_batch_size());

        while !logic.stopped() {
            logic.set_current_backlog_size(self.ledger.backlog_size());
            logic = self
                .logic
                .wait_timeout_while(logic, Duration::from_secs(1), |i| {
                    !i.stopped() && !i.rollback_needed()
                })
                .0;

            logic = self.run_one(logic, &mut targets);
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
                .roll_back_batch(&*targets, target_count as usize);

            state = self.logic.lock();
        }

        state
    }

    fn unconfirmed_accounts_found(&self, batch: &[UnconfirmedInfo]) {
        let mut walker = AccountWalker::new(&self.ledger);
        for info in batch {
            walker.walk_backwards(
                info.account_info.head,
                info.conf_info.frontier,
                |block, priority| self.logic.lock().insert(&block, priority),
            );
        }
    }
}

impl StatsSource for BoundedBacklog {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.logic.lock().collect_stats(result);
    }
}

impl ContainerInfoProvider for BoundedBacklog {
    fn container_info(&self) -> ContainerInfo {
        self.logic.lock().container_info()
    }
}

impl EventHandler<LedgerPipelineEvent> for BoundedBacklog {
    fn handle(&self, event: &LedgerPipelineEvent) {
        match event {
            LedgerPipelineEvent::Ledger(event) => match event {
                LedgerEvent::BlocksProcessed(results) => {
                    self.logic.lock().insert_processed(results);
                }
                LedgerEvent::BlocksConfirmed(confirmed) => {
                    self.logic
                        .lock()
                        .remove_batch(confirmed.iter().map(|i| i.0.hash()));
                }
                LedgerEvent::BlocksRolledBack(rolled_back) => {
                    self.logic.lock().remove_batch(rolled_back.hashes());
                }
            },
            LedgerPipelineEvent::UnconfirmedFound(unconfirmed) => {
                self.unconfirmed_accounts_found(unconfirmed);
            }
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_cooldown_sets_flag() {
        let backlog = BoundedBacklog::new_null();
        backlog.set_cooldown(true);
        assert!(backlog.logic.lock().cool_down());
    }
}
