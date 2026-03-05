use std::{
    cmp::min,
    sync::{Arc, atomic::Ordering::Relaxed},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;

use crate::{
    block_processing::{
        BoundedBacklogConfig, backlog_index::BacklogIndex,
        bounded_backlog::stats::BoundedBacklogStats,
    },
    consensus::election_schedulers::priority::prio_bucket_count,
};

/// Continuously rolls back unconfirmed blocks with the lowest priority
/// if the backlog exceeds the configured limit
pub(crate) struct RollbackLoop {
    pub(super) state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    pub(crate) stats: Arc<BoundedBacklogStats>,
    pub(super) ledger: Arc<Ledger>,
    pub(super) can_roll_back: Box<dyn Fn(&BlockHash) -> bool + Send + Sync>,
}

impl RollbackLoop {
    pub(crate) fn run_process(&self) {
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

pub(crate) struct BoundedBacklogState {
    pub(crate) stopped: bool,
    pub(crate) cool_down: bool,
    pub(crate) index: BacklogIndex,
    config: BoundedBacklogConfig,
    bucket_count: usize,
    backlog_size: u64,
}

impl BoundedBacklogState {
    pub(crate) fn new(config: BoundedBacklogConfig) -> Self {
        Self {
            stopped: false,
            cool_down: false,
            index: BacklogIndex::new(prio_bucket_count()),
            config,
            bucket_count: prio_bucket_count(),
            backlog_size: 0,
        }
    }

    pub(crate) fn rollback_needed(&self) -> bool {
        if self.cool_down {
            return false;
        }

        // Both ledger and tracked backlog must be over the threshold
        let max_backlog = self.config.max_backlog;
        self.backlog_size > max_backlog && self.index.len() > max_backlog as usize
    }

    fn target_count(&self) -> u64 {
        self.backlog_size.saturating_sub(self.config.max_backlog)
    }

    fn batch_size(&self) -> usize {
        min(self.target_count(), self.config.batch_size as u64) as usize
    }

    fn gather_targets(&self, can_rollback: impl Fn(&BlockHash) -> bool) -> Vec<BlockHash> {
        let mut targets = Vec::new();

        let max_count = self.batch_size();
        // Start rolling back from lowest index buckets first
        for bucket in 0..self.bucket_count {
            // Only start rolling back if the bucket is over the threshold of unconfirmed blocks
            if self.index.len_of_bucket(bucket) > self.bucket_threshold() {
                let count = min(max_count, self.config.batch_size);
                let top = self.index.top(bucket, count, |hash| {
                    // Only rollback if the block is not being used by the node
                    can_rollback(hash)
                });
                targets.extend(top);
            }
        }
        targets
    }

    fn bucket_threshold(&self) -> usize {
        self.config.max_backlog as usize / self.bucket_count
    }
}
