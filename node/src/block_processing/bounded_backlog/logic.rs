use std::cmp::min;

use rsnano_ledger::ProcessResult;
use rsnano_types::{BlockHash, BlockPriority, SavedBlock};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use crate::{
    block_processing::bounded_backlog::index::BacklogEntry,
    consensus::election_schedulers::priority::{prio_bucket_count, prio_bucket_index},
};

use super::index::BacklogIndex;

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
    index: BacklogIndex,
    config: BoundedBacklogConfig,
    block_count: u64,
    confirmed_count: u64,
    bootstrap_weights_max_blocks: u64,

    // stats
    pub rollback_iterations: u64,
    total_gathered: u64,
}

impl BoundedBacklogLogic {
    pub(crate) fn new(config: BoundedBacklogConfig) -> Self {
        Self {
            stopped: false,
            cool_down: false,
            index: BacklogIndex::new(prio_bucket_count()),
            config,
            block_count: 0,
            confirmed_count: 0,
            bootstrap_weights_max_blocks: 0,
            rollback_iterations: 0,
            total_gathered: 0,
        }
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped
    }

    pub(crate) fn stop(&mut self) {
        self.stopped = true;
    }

    pub(crate) fn set_cooldown(&mut self, cool_down: bool) {
        self.cool_down = cool_down;
    }

    #[cfg(test)]
    pub(crate) fn cool_down(&self) -> bool {
        self.cool_down
    }

    pub(crate) fn backlog_size(&self) -> u64 {
        self.block_count.saturating_sub(self.confirmed_count)
    }

    #[cfg(test)]
    pub(crate) fn block_count(&self) -> u64 {
        self.block_count
    }

    #[cfg(test)]
    pub(crate) fn confirmed_count(&self) -> u64 {
        self.confirmed_count
    }

    pub(crate) fn set_ledger_info(&mut self, block_count: u64, confirmed_count: u64) {
        self.block_count = block_count;
        self.confirmed_count = confirmed_count;
    }

    pub(crate) fn set_bootstrap_weights_max_blocks(&mut self, max_blocks: u64) {
        self.bootstrap_weights_max_blocks = max_blocks;
    }

    #[cfg(test)]
    pub(crate) fn bootstrap_weights_max_blocks(&self) -> u64 {
        self.bootstrap_weights_max_blocks
    }

    pub(crate) fn max_backlog(&self) -> u64 {
        self.config.max_backlog
    }

    pub(crate) fn should_throttle_block_processor(&self) -> bool {
        self.backlog_size() as f64 > self.max_backlog() as f64 * 1.5
    }

    pub(crate) fn rollback_batch_size(&self) -> usize {
        self.config.rollback_batch_size
    }

    pub(crate) fn rollback_needed(&self) -> bool {
        if self.cool_down {
            return false;
        }

        // Both ledger and tracked backlog must be over the threshold
        let max_backlog = self.config.max_backlog;
        self.backlog_size() > max_backlog && self.index.len() > max_backlog as usize
    }

    /// The number of rollbacks required in order to reach the max allowed backlog
    pub(crate) fn rollback_target_count(&self) -> u64 {
        self.backlog_size().saturating_sub(self.config.max_backlog)
    }

    fn next_rollback_batch_size(&self) -> usize {
        min(
            self.rollback_target_count(),
            self.config.rollback_batch_size as u64,
        ) as usize
    }

    pub(crate) fn gather_targets(&mut self, targets: &mut Vec<BlockHash>) {
        self.rollback_iterations += 1;
        targets.clear();
        let batch_size = self.next_rollback_batch_size();

        // Start rolling back from lowest index buckets first
        for bucket in 0..self.index.bucket_count() {
            // Only start rolling back if the bucket is over the threshold of unconfirmed blocks
            if self.index.len_of_bucket(bucket) > self.bucket_threshold() {
                let count = batch_size - targets.len();
                self.index.drain_lowest_priority(bucket, count, targets);
                if targets.len() >= batch_size {
                    break;
                }
            }
        }
        self.total_gathered += targets.len() as u64;
    }

    fn bucket_threshold(&self) -> usize {
        self.config.max_backlog as usize / self.index.bucket_count()
    }

    pub(crate) fn remove_batch(&mut self, accounts: impl IntoIterator<Item = BlockHash>) {
        for account in accounts.into_iter() {
            self.index.remove(&account);
        }
    }

    /// Track newly inserted blocks, wich are unconfirmed
    pub(crate) fn insert_processed(&mut self, batch: &[ProcessResult]) {
        for result in batch {
            if result.status.is_ok()
                && let Some(block) = &result.saved_block
            {
                self.insert(block, result.priority);
            }
        }
    }

    /// Returns false, if the block wasn't inserted, because it was already in the index
    pub(crate) fn insert(&mut self, block: &SavedBlock, priority: BlockPriority) -> bool {
        let bucket_index = prio_bucket_index(priority.balance);
        self.index.insert(BacklogEntry {
            hash: block.hash(),
            bucket_index,
            priority: priority.time,
        })
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, hash: &BlockHash) -> bool {
        self.index.contains(hash)
    }
}

impl StatsSource for BoundedBacklogLogic {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "bounded_backlog",
            "rollback_iterations",
            self.rollback_iterations,
        );
        result.insert("bounded_backlog", "gathered_targets", self.total_gathered);
    }
}

impl ContainerInfoProvider for BoundedBacklogLogic {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .leaf("backlog", self.index.len(), 0)
            .node("index", self.index.container_info())
            .finish()
    }
}

impl Default for BoundedBacklogLogic {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_ledger::{BlockError, BlockSource, ProcessResult};
    use rsnano_types::{Block, BlockHash, BlockPriority, PrivateKey, SavedBlock};
    use rsnano_utils::container_info::ContainerInfoEntry;

    /*
     * Flags / state
     */

    #[test]
    fn stopped_initially_false() {
        assert!(!BoundedBacklogLogic::default().stopped());
    }

    #[test]
    fn stop_sets_flag() {
        let mut logic = BoundedBacklogLogic::default();
        logic.stop();
        assert!(logic.stopped());
    }

    #[test]
    fn cool_down_initially_false() {
        assert!(!BoundedBacklogLogic::default().cool_down());
    }

    #[test]
    fn set_cool_down_sets_flag() {
        let mut logic = BoundedBacklogLogic::default();
        logic.set_cooldown(true);
        assert!(logic.cool_down());
    }

    /*
     * Backlog size
     */

    #[test]
    fn current_backlog_size_initially_zero() {
        assert_eq!(BoundedBacklogLogic::default().backlog_size(), 0);
    }

    #[test]
    fn set_ledger_info() {
        let mut logic = BoundedBacklogLogic::default();
        logic.set_ledger_info(43, 1);
        assert_eq!(logic.block_count(), 43);
        assert_eq!(logic.confirmed_count(), 1);
        assert_eq!(logic.backlog_size(), 42);
    }

    /*
     * Config accessors
     */

    #[test]
    fn max_backlog_returns_configured_value() {
        let config = BoundedBacklogConfig {
            max_backlog: 12345,
            rollback_batch_size: 32,
        };
        assert_eq!(BoundedBacklogLogic::new(config).max_backlog(), 12345);
    }

    #[test]
    fn rollback_batch_size_returns_configured_value() {
        let config = BoundedBacklogConfig {
            max_backlog: 100,
            rollback_batch_size: 7,
        };
        assert_eq!(BoundedBacklogLogic::new(config).rollback_batch_size(), 7);
    }

    /*
     * rollback_needed
     */

    #[test]
    fn rollback_not_needed_when_under_threshold() {
        assert!(!BoundedBacklogLogic::default().rollback_needed());
    }

    #[test]
    fn rollback_not_needed_when_cool_down_active() {
        let mut logic = BoundedBacklogLogic::new(small_config());
        logic.insert(&make_block(1), BlockPriority::new_test_instance());
        logic.insert(&make_block(2), BlockPriority::new_test_instance());
        logic.set_ledger_info(4, 1);
        logic.set_cooldown(true);
        assert!(!logic.rollback_needed());
    }

    #[test]
    fn rollback_not_needed_when_only_backlog_size_is_over() {
        let mut logic = BoundedBacklogLogic::new(small_config());
        logic.set_ledger_info(4, 1);
        assert!(!logic.rollback_needed());
    }

    #[test]
    fn rollback_not_needed_when_only_index_is_over() {
        let mut logic = BoundedBacklogLogic::new(small_config());
        logic.insert(&make_block(1), BlockPriority::new_test_instance());
        logic.insert(&make_block(2), BlockPriority::new_test_instance());
        assert!(!logic.rollback_needed());
    }

    #[test]
    fn rollback_needed_when_both_over_threshold() {
        let mut logic = BoundedBacklogLogic::new(small_config());
        logic.insert(&make_block(1), BlockPriority::new_test_instance());
        logic.insert(&make_block(2), BlockPriority::new_test_instance());
        logic.set_ledger_info(4, 1);
        assert!(logic.rollback_needed());
    }

    /*
     * rollback_target_count
     */

    #[test]
    fn rollback_target_count_zero_when_at_or_below_max() {
        let config = BoundedBacklogConfig {
            max_backlog: 5,
            rollback_batch_size: 10,
        };
        let mut logic = BoundedBacklogLogic::new(config);
        logic.set_ledger_info(6, 1);
        assert_eq!(logic.rollback_target_count(), 0);
    }

    #[test]
    fn rollback_target_count_correct_when_over_max() {
        let config = BoundedBacklogConfig {
            max_backlog: 3,
            rollback_batch_size: 10,
        };
        let mut logic = BoundedBacklogLogic::new(config);
        logic.set_ledger_info(6, 1);
        assert_eq!(logic.rollback_target_count(), 2);
    }

    /*
     * insert / contains
     */

    #[test]
    fn insert_returns_true_on_first_insert() {
        let mut logic = BoundedBacklogLogic::default();
        assert!(logic.insert(&make_block(1), BlockPriority::new_test_instance()));
    }

    #[test]
    fn insert_returns_false_on_duplicate() {
        let mut logic = BoundedBacklogLogic::default();
        let block = make_block(1);
        logic.insert(&block, BlockPriority::new_test_instance());
        assert!(!logic.insert(&block, BlockPriority::new_test_instance()));
    }

    #[test]
    fn contains_returns_true_after_insert() {
        let mut logic = BoundedBacklogLogic::default();
        let block = make_block(1);
        logic.insert(&block, BlockPriority::new_test_instance());
        assert!(logic.contains(&block.hash()));
    }

    #[test]
    fn contains_returns_false_for_unknown_hash() {
        assert!(!BoundedBacklogLogic::default().contains(&BlockHash::from(1)));
    }

    /*
     * insert_processed
     */

    #[test]
    fn insert_processed_inserts_ok_blocks_with_saved_block() {
        let mut logic = BoundedBacklogLogic::default();
        let block = make_block(1);
        logic.insert_processed(&[make_result(block.clone())]);
        assert!(logic.contains(&block.hash()));
    }

    #[test]
    fn insert_processed_skips_error_status() {
        let mut logic = BoundedBacklogLogic::default();
        let block = make_block(1);
        let result = ProcessResult {
            block: Block::new_test_instance(),
            source: BlockSource::Live,
            status: Err(BlockError::GapPrevious),
            saved_block: Some(block.clone()),
            priority: BlockPriority::new_test_instance(),
        };
        logic.insert_processed(&[result]);
        assert!(!logic.contains(&block.hash()));
    }

    /*
     * remove_batch
     */

    #[test]
    fn remove_batch_removes_inserted_blocks() {
        let mut logic = BoundedBacklogLogic::default();
        let block = make_block(1);
        logic.insert(&block, BlockPriority::new_test_instance());
        logic.remove_batch(std::iter::once(block.hash()));
        assert!(!logic.contains(&block.hash()));
    }

    #[test]
    fn remove_batch_ignores_unknown_hashes() {
        let mut logic = BoundedBacklogLogic::default();
        logic.remove_batch(std::iter::once(BlockHash::from(999)));
        // should not panic
    }

    /*
     * gather_targets
     */

    #[test]
    fn gather_targets_increments_gather_called() {
        let mut logic = BoundedBacklogLogic::new(small_config());
        let mut targets = Vec::new();
        logic.gather_targets(&mut targets);
        assert_eq!(logic.rollback_iterations, 1);
    }

    #[test]
    fn gather_targets_increments_total_gathered() {
        let mut logic = BoundedBacklogLogic::new(small_config());
        logic.insert(&make_block(1), BlockPriority::new_test_instance());
        logic.insert(&make_block(2), BlockPriority::new_test_instance());
        logic.set_ledger_info(4, 1);
        let mut targets = Vec::new();
        logic.gather_targets(&mut targets);
        assert_eq!(logic.total_gathered, 2);
    }

    #[test]
    fn gather_targets_clears_existing_targets() {
        let mut logic = BoundedBacklogLogic::new(small_config());
        let mut targets = vec![BlockHash::from(1), BlockHash::from(2)];
        logic.gather_targets(&mut targets);
        assert!(!targets.contains(&BlockHash::from(1)));
        assert!(!targets.contains(&BlockHash::from(2)));
    }

    #[test]
    fn gather_targets_empty_when_no_blocks_in_index() {
        let mut logic = BoundedBacklogLogic::new(small_config());
        logic.set_ledger_info(11, 1);
        let mut targets = Vec::new();
        logic.gather_targets(&mut targets);
        assert!(targets.is_empty());
    }

    #[test]
    fn gather_targets_respects_rollback_batch_size() {
        let config = BoundedBacklogConfig {
            max_backlog: 1,
            rollback_batch_size: 2,
        };
        let mut logic = BoundedBacklogLogic::new(config);
        for i in 1..=5 {
            logic.insert(&make_block(i), BlockPriority::new_test_instance());
        }
        logic.set_ledger_info(11, 1);
        let mut targets = Vec::new();
        logic.gather_targets(&mut targets);
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn gather_targets_skips_buckets_under_threshold() {
        // With a large max_backlog the bucket threshold is high,
        // so a bucket with only 1 entry is skipped
        let config = BoundedBacklogConfig {
            max_backlog: 1_000_000,
            rollback_batch_size: 10,
        };
        let mut logic = BoundedBacklogLogic::new(config);
        logic.insert(&make_block(1), BlockPriority::new_test_instance());
        logic.set_ledger_info(1_000_002, 1);
        let mut targets = Vec::new();
        logic.gather_targets(&mut targets);
        assert!(targets.is_empty());
    }

    /*
     * Observability
     */

    #[test]
    fn container_info_shows_backlog_count() {
        let mut logic = BoundedBacklogLogic::default();
        logic.insert(&make_block(1), BlockPriority::new_test_instance());
        logic.insert(&make_block(2), BlockPriority::new_test_instance());
        let ContainerInfoEntry::Leaf(leaf) = &logic.container_info()[0] else {
            panic!("expected leaf");
        };
        assert_eq!(leaf.info.count, 2);
    }

    #[test]
    fn collects_stats() {
        let mut logic = BoundedBacklogLogic::new(Default::default());
        logic.rollback_iterations = 10;
        logic.total_gathered = 11;

        let mut result = StatsCollection::new();
        logic.collect_stats(&mut result);

        assert_eq!(result.get("bounded_backlog", "rollback_iterations"), 10);
        assert_eq!(result.get("bounded_backlog", "gathered_targets"), 11);
    }

    /*
     * Throttling
     */

    #[test]
    fn throttle_block_processor() {
        let mut logic = BoundedBacklogLogic::new(BoundedBacklogConfig {
            max_backlog: 100,
            rollback_batch_size: 1,
        });
        assert!(!logic.should_throttle_block_processor());
        logic.set_ledger_info(151, 1);
        assert!(!logic.should_throttle_block_processor());
        logic.set_ledger_info(152, 1);
        assert!(logic.should_throttle_block_processor());
        logic.set_ledger_info(101, 1);
        assert!(!logic.should_throttle_block_processor());
    }

    /*
     * Test helpers
     */

    fn make_block(key: u64) -> SavedBlock {
        SavedBlock::new_test_instance_with_key(PrivateKey::from(key))
    }

    fn make_result(block: SavedBlock) -> ProcessResult {
        ProcessResult {
            block: Block::new_test_instance(),
            source: BlockSource::Live,
            status: Ok(()),
            saved_block: Some(block),
            priority: BlockPriority::new_test_instance(),
        }
    }

    /// Config where bucket_threshold = 0 so all non-empty buckets qualify for gather
    fn small_config() -> BoundedBacklogConfig {
        BoundedBacklogConfig {
            max_backlog: 1,
            rollback_batch_size: 10,
        }
    }
}
