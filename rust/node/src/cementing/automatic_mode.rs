use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use rsnano_core::{utils::ContainerInfoComponent, BlockEnum};
use rsnano_ledger::Ledger;

use super::{
    block_cache::BlockCache, BoundedMode, BoundedModeContainerInfo, UnboundedMode,
    UnboundedModeContainerInfo,
};

#[derive(FromPrimitive, Clone, PartialEq, Eq, Copy)]
pub enum ConfirmationHeightMode {
    Automatic,
    Unbounded,
    Bounded,
}

/// When the uncemented count (block count - cemented count) is less than this use the unbounded processor
pub(super) const UNBOUNDED_CUTOFF: usize = 16384;

pub(super) struct AutomaticMode {
    pub bounded_mode: BoundedMode,
    pub unbounded_mode: UnboundedMode,
    pub mode: ConfirmationHeightMode,
    pub ledger: Arc<Ledger>,
}

impl AutomaticMode {
    pub fn pending_writes_empty(&self) -> bool {
        self.bounded_mode.pending_writes_empty() && self.unbounded_mode.pending_writes_empty()
    }

    pub fn write_pending_blocks(&mut self) {
        if !self.bounded_mode.pending_writes_empty() {
            self.bounded_mode.write_pending_blocks();
        } else if !self.unbounded_mode.pending_writes_empty() {
            self.unbounded_mode.write_pending_blocks();
        }
    }

    pub fn process(&mut self, block: Arc<BlockEnum>) {
        if self.should_use_unbounded_processor() {
            self.unbounded_mode.process(block);
        } else {
            self.bounded_mode.process(&block);
        }
    }

    pub fn clear_process_vars(&mut self) {
        self.bounded_mode.clear_process_vars();
        self.unbounded_mode.clear_process_vars();
    }

    pub fn container_info(&self) -> AutomaticModeContainerInfo {
        AutomaticModeContainerInfo {
            bounded_container_info: self.bounded_mode.container_info(),
            unbounded_container_info: self.unbounded_mode.container_info(),
        }
    }

    pub fn block_cache(&self) -> &Arc<BlockCache> {
        self.unbounded_mode.block_cache()
    }

    pub fn batch_write_size(&self) -> &Arc<AtomicU64> {
        &self.bounded_mode.batch_write_size
    }

    fn should_use_unbounded_processor(&self) -> bool {
        self.force_unbounded() || self.valid_unbounded()
    }

    fn valid_unbounded(&self) -> bool {
        self.mode == ConfirmationHeightMode::Automatic
            && self.are_blocks_within_automatic_unbounded_section()
            && self.bounded_mode.pending_writes_empty()
    }

    fn force_unbounded(&self) -> bool {
        !self.unbounded_mode.pending_writes_empty()
            || self.mode == ConfirmationHeightMode::Unbounded
    }

    fn are_blocks_within_automatic_unbounded_section(&self) -> bool {
        let block_count = self.ledger.cache.block_count.load(Ordering::SeqCst);
        let cemented_count = self.ledger.cache.cemented_count.load(Ordering::SeqCst);

        block_count < (UNBOUNDED_CUTOFF as u64)
            || block_count - (UNBOUNDED_CUTOFF as u64) < cemented_count
    }
}

pub(super) struct AutomaticModeContainerInfo {
    unbounded_container_info: UnboundedModeContainerInfo,
    bounded_container_info: BoundedModeContainerInfo,
}

impl AutomaticModeContainerInfo {
    pub fn collect(&self) -> Vec<ContainerInfoComponent> {
        vec![
            self.bounded_container_info.collect(),
            self.unbounded_container_info.collect(),
        ]
    }
}
