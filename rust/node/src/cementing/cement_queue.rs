use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use super::ConfHeightDetails;

/// Queue for blocks that will be cemented
pub(super) struct CementQueue {
    pending_writes: VecDeque<ConfHeightDetails>,
    pending_writes_size: Arc<AtomicUsize>,
}

impl CementQueue {
    pub(crate) fn new() -> Self {
        Self {
            pending_writes: VecDeque::new(),
            pending_writes_size: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pending_writes.is_empty()
    }

    pub fn push(&mut self, details: ConfHeightDetails) {
        self.pending_writes.push_back(details);
        self.pending_writes_size.fetch_add(1, Ordering::Relaxed);
    }

    pub fn pop(&mut self) -> Option<ConfHeightDetails> {
        let result = self.pending_writes.pop_front();
        if result.is_some() {
            self.pending_writes_size.fetch_sub(1, Ordering::Relaxed);
        }
        result
    }

    pub fn len(&self) -> usize {
        self.pending_writes.len()
    }

    pub fn total_cemented_blocks(&self) -> u64 {
        self.pending_writes
            .iter()
            .map(|x| x.update_height.num_blocks_cemented)
            .sum()
    }

    pub fn atomic_len(&self) -> &Arc<AtomicUsize> {
        &self.pending_writes_size
    }
}