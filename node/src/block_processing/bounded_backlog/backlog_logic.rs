use std::cmp::min;

use rsnano_types::BlockHash;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use crate::{
    block_processing::{BoundedBacklogConfig, backlog_index::BacklogIndex},
    consensus::election_schedulers::priority::prio_bucket_count,
};

pub(crate) struct BoundedBacklogLogic {
    pub(crate) stopped: bool,
    pub(crate) cool_down: bool,
    pub(crate) index: BacklogIndex,
    pub(crate) config: BoundedBacklogConfig,
    bucket_count: usize,
    pub(crate) backlog_size: u64,

    // stats
    pub(crate) gather_called: u64,
    pub(crate) total_gathered: u64,
}

impl BoundedBacklogLogic {
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
    pub(crate) fn rollback_target_count(&self) -> u64 {
        self.backlog_size.saturating_sub(self.config.max_backlog)
    }

    fn rollback_batch_size(&self) -> usize {
        min(
            self.rollback_target_count(),
            self.config.rollback_batch_size as u64,
        ) as usize
    }

    pub(crate) fn gather_targets(&mut self, targets: &mut Vec<BlockHash>) {
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
