use std::{
    sync::{Arc, atomic::Ordering::Relaxed},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;

use super::BoundedBacklogState;
use crate::block_processing::bounded_backlog::stats::BoundedBacklogStats;

/// Continuously rolls back unconfirmed blocks with the lowest priority
/// if the backlog exceeds the configured limit
pub(super) struct RollbackLoop {
    pub(super) state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    pub(crate) stats: Arc<BoundedBacklogStats>,
    pub(super) ledger: Arc<Ledger>,
    pub(super) can_roll_back: Box<dyn Fn(&BlockHash) -> bool + Send + Sync>,
}

impl RollbackLoop {
    pub(super) fn run_process(&self) {
        let mut state = self.state.lock();
        while !state.stopped {
            state.backlog_size = self.ledger.backlog_size();
            state = self
                .state
                .wait_timeout_while(state, Duration::from_secs(1), |i| {
                    !i.stopped && !i.rollback_needed()
                })
                .0;

            if state.stopped {
                return;
            }

            if !state.rollback_needed() {
                continue;
            }

            self.stats.loop_rollback.fetch_add(1, Relaxed);

            let targets = state.gather_targets(&self.can_roll_back);

            if !targets.is_empty() {
                let target_count = state.target_count();
                drop(state);
                self.stats
                    .gathered_targets
                    .fetch_add(targets.len(), Relaxed);

                let processed =
                    self.roll_back(&targets, target_count as usize, &self.can_roll_back);
                state = self.state.lock();

                // Erase rolled back blocks from the index
                for hash in &processed {
                    state.index.erase_hash(hash);
                }
            }
        }
    }

    fn roll_back(
        &self,
        targets: &[BlockHash],
        max_rollbacks: usize,
        can_roll_back: impl Fn(&BlockHash) -> bool,
    ) -> Vec<BlockHash> {
        let results = self
            .ledger
            .roll_back_batch(targets, max_rollbacks, can_roll_back);

        // TODO: listen for LedgerEvent::BlocksRolledBack instead of returning the rolled back
        // blocks from ledger?
        let mut processed_hashes = Vec::new();
        for result in results.iter() {
            if result.rolled_back.is_empty() {
                processed_hashes.push(result.target_hash);
            } else {
                for h in &result.rolled_back {
                    processed_hashes.push(h.hash());
                }
            }
        }

        processed_hashes
    }
}
