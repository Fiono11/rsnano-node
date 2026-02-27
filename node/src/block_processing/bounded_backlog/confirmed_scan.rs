use std::sync::{Arc, atomic::Ordering::Relaxed};

use rsnano_ledger::{Ledger, LedgerSet};
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;

use crate::block_processing::bounded_backlog::{BoundedBacklogState, stats::BoundedBacklogStats};

/// Scans the bounded backlog index for recently confirmed blocks and removes those from the index
pub(crate) struct RecentlyConfirmedScan {
    state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    stats2: Arc<BoundedBacklogStats>,
    batch_size: usize,
    last: BlockHash,
    confirmed: Vec<BlockHash>,

    // Infrastructure:
    ledger: Arc<Ledger>,
}

impl RecentlyConfirmedScan {
    pub(crate) fn new(
        state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
        stats: Arc<BoundedBacklogStats>,
        ledger: Arc<Ledger>,
        batch_size: usize,
    ) -> Self {
        Self {
            state,
            stats2: stats,
            ledger,
            batch_size,
            last: BlockHash::ZERO,
            confirmed: Vec::with_capacity(batch_size),
        }
    }

    pub(crate) fn scan_batch(&mut self) {
        self.stats2.loop_scan.fetch_add(1, Relaxed);

        let batch = self.state.lock().index.next(&self.last, self.batch_size);

        self.stats2.scanned.fetch_add(batch.len(), Relaxed);

        // If batch is empty, we iterated over all accounts in the index
        self.last = batch.last().cloned().unwrap_or_default();

        if !batch.is_empty() {
            self.check_confirmed(&batch);

            if !self.confirmed.is_empty() {
                self.state.lock().index.erase_hashes(&self.confirmed);
                self.state.notify_all();
            }
        }
    }

    fn check_confirmed(&mut self, hashes: &[BlockHash]) {
        self.confirmed.clear();
        let unconfirmed = self.ledger.unconfirmed();
        for hash in hashes {
            // Erase if the block is either confirmed or missing
            if !unconfirmed.block_exists(&hash) {
                self.confirmed.push(*hash);
            }
        }
    }
}
