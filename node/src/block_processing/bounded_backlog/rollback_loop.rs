use std::{
    cmp::min,
    sync::{Arc, MutexGuard},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;

use crate::{
    block_processing::{BoundedBacklogConfig, backlog_index::BacklogIndex},
    consensus::election_schedulers::priority::prio_bucket_count,
};
use rsnano_utils::stats::{StatsCollection, StatsSource};

/// Continuously rolls back unconfirmed blocks with the lowest priority
/// if the backlog exceeds the configured limit
pub(crate) struct RollbackLoop {
    pub(super) state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    pub(super) ledger: Arc<Ledger>,
    pub(super) can_roll_back: Box<dyn Fn(&BlockHash) -> bool + Send + Sync>,
}

impl RollbackLoop {
    pub(crate) fn run(&self) {
        let mut state = self.state.lock();
        let mut targets = Vec::with_capacity(state.config.rollback_batch_size);

        while !state.stopped {
            state.backlog_size = self.ledger.backlog_size();
            state = self
                .state
                .wait_timeout_while(state, Duration::from_secs(1), |i| {
                    !i.stopped && !i.rollback_needed()
                })
                .0;

            state = self.run_one(state, &mut targets);
        }
    }

    fn run_one<'a>(
        &'a self,
        mut state: MutexGuard<'a, BoundedBacklogState>,
        targets: &mut Vec<BlockHash>,
    ) -> MutexGuard<'a, BoundedBacklogState> {
        if state.stopped || !state.rollback_needed() {
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

pub(crate) struct BoundedBacklogState {
    pub(crate) stopped: bool,
    pub(crate) cool_down: bool,
    pub(crate) index: BacklogIndex,
    config: BoundedBacklogConfig,
    bucket_count: usize,
    backlog_size: u64,

    // stats
    gather_called: u64,
    total_gathered: u64,
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
            gather_called: 0,
            total_gathered: 0,
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

    /// The number of rollbacks required in order to reach the max allowed backlog
    fn rollback_target_count(&self) -> u64 {
        self.backlog_size.saturating_sub(self.config.max_backlog)
    }

    fn rollback_batch_size(&self) -> usize {
        min(
            self.rollback_target_count(),
            self.config.rollback_batch_size as u64,
        ) as usize
    }

    fn gather_targets(&mut self, targets: &mut Vec<BlockHash>) {
        self.gather_called += 1;
        targets.clear();
        let batch_size = self.rollback_batch_size();

        // Start rolling back from lowest index buckets first
        for bucket in 0..self.bucket_count {
            // Only start rolling back if the bucket is over the threshold of unconfirmed blocks
            if self.index.len_of_bucket(bucket) > self.bucket_threshold() {
                let count = batch_size - targets.len();
                self.index.drain_top(bucket, count, targets);
                if targets.len() >= batch_size {
                    break;
                }
            }
        }
        self.total_gathered += targets.len() as u64;
    }

    fn bucket_threshold(&self) -> usize {
        self.config.max_backlog as usize / self.bucket_count
    }
}

impl StatsSource for BoundedBacklogState {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert("bounded_backlog", "loop", self.gather_called);
        result.insert("bounded_backlog", "gathered_targets", self.total_gathered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_stats() {
        let mut state = BoundedBacklogState::new(Default::default());
        state.gather_called = 10;
        state.total_gathered = 11;

        let mut result = StatsCollection::new();
        state.collect_stats(&mut result);

        assert_eq!(result.get("bounded_backlog", "loop"), 10);
        assert_eq!(result.get("bounded_backlog", "gathered_targets"), 11);
    }
}
