use std::cmp::min;

use rsnano_types::BlockHash;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use crate::{
    block_processing::backlog_index::BacklogIndex,
    consensus::election_schedulers::priority::prio_bucket_count,
};

#[derive(Clone, Debug, PartialEq)]
pub struct BoundedBacklogConfig {
    /// The maximum allowed count of unconfirmed blocks, before the bounded backlog
    /// starts rolling back blocks
    pub max_backlog: u64,

    /// The rollback is done in batches of this configured size
    pub rollback_batch_size: usize,
}

impl Default for BoundedBacklogConfig {
    fn default() -> Self {
        Self {
            max_backlog: 100_000,
            rollback_batch_size: 32,
        }
    }
}

pub(crate) struct BoundedBacklogLogic {
    stopped: bool,
    cool_down: bool,
    pub(crate) index: BacklogIndex,
    pub(crate) config: BoundedBacklogConfig,
    bucket_count: usize,
    current_backlog_size: u64,

    // stats
    gather_called: u64,
    total_gathered: u64,
}

impl BoundedBacklogLogic {
    pub(crate) fn new(config: BoundedBacklogConfig) -> Self {
        Self {
            stopped: false,
            cool_down: false,
            index: BacklogIndex::new(prio_bucket_count()),
            config,
            bucket_count: prio_bucket_count(),
            current_backlog_size: 0,
            gather_called: 0,
            total_gathered: 0,
        }
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped
    }

    pub(crate) fn stop(&mut self) {
        self.stopped = true;
    }

    pub(crate) fn set_cool_down(&mut self, cool_down: bool) {
        self.cool_down = cool_down;
    }

    pub(crate) fn set_current_backlog_size(&mut self, size: u64) {
        self.current_backlog_size = size;
    }

    pub(crate) fn rollback_needed(&self) -> bool {
        if self.cool_down {
            return false;
        }

        // Both ledger and tracked backlog must be over the threshold
        let max_backlog = self.config.max_backlog;
        self.current_backlog_size > max_backlog && self.index.len() > max_backlog as usize
    }

    /// The number of rollbacks required in order to reach the max allowed backlog
    pub(crate) fn rollback_target_count(&self) -> u64 {
        self.current_backlog_size
            .saturating_sub(self.config.max_backlog)
    }

    fn next_rollback_batch_size(&self) -> usize {
        min(
            self.rollback_target_count(),
            self.config.rollback_batch_size as u64,
        ) as usize
    }

    pub(crate) fn gather_targets(&mut self, targets: &mut Vec<BlockHash>) {
        self.gather_called += 1;
        targets.clear();
        let batch_size = self.next_rollback_batch_size();

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

impl StatsSource for BoundedBacklogLogic {
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
        let mut logic = BoundedBacklogLogic::new(Default::default());
        logic.gather_called = 10;
        logic.total_gathered = 11;

        let mut result = StatsCollection::new();
        logic.collect_stats(&mut result);

        assert_eq!(result.get("bounded_backlog", "loop"), 10);
        assert_eq!(result.get("bounded_backlog", "gathered_targets"), 11);
    }
}
