use std::sync::{Arc, atomic::Ordering::Relaxed};

use rsnano_ledger::{Ledger, LedgerSet};
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;

use crate::block_processing::bounded_backlog::{BoundedBacklogState, stats::BoundedBacklogStats};

/// Scans the bounded backlog index for recently confirmed blocks and removes those from the index
pub(crate) struct RecentlyConfirmedScan {
    state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    stats: Arc<BoundedBacklogStats>,
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
            stats,
            ledger,
            batch_size,
            last: BlockHash::ZERO,
            confirmed: Vec::with_capacity(batch_size),
        }
    }

    pub(crate) fn scan_batch(&mut self) {
        self.stats.loop_scan.fetch_add(1, Relaxed);
        let batch = self.state.lock().index.next(&self.last, self.batch_size);
        self.stats.scanned.fetch_add(batch.len(), Relaxed);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_processing::{
        BoundedBacklogConfig,
        backlog_index::{BacklogEntry, BacklogIndex},
    };
    use rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder;
    use rsnano_types::Account;

    #[test]
    fn does_nothing_when_index_is_empty() {
        let mut scan = create_test_scan(test_state(), Ledger::new_null());

        scan.scan_batch();

        let (index, stats) = dismantle(scan);
        assert_eq!(stats.loop_scan.load(Relaxed), 1, "loop count");
        assert_eq!(stats.scanned.load(Relaxed), 0, "scanned count");
        assert!(index.is_empty(), "index should be empty after scan");
    }

    #[test]
    fn removes_hash_from_index_if_hash_is_unknown() {
        let mut state = test_state();
        state.index.insert(BacklogEntry::new_test_instance());
        let mut scan = create_test_scan(state, Ledger::new_null());

        scan.scan_batch();

        let (index, stats) = dismantle(scan);
        assert_eq!(stats.loop_scan.load(Relaxed), 1, "loop count");
        assert_eq!(stats.scanned.load(Relaxed), 1, "scanned count");
        assert!(index.is_empty(), "entry should have been removed");
    }

    #[test]
    fn removes_hash_from_index_if_hash_is_confirmed() {
        let mut state = test_state();
        let ledger = Ledger::new_null();
        let mut builder = UnsavedBlockLatticeBuilder::with_stub_work();
        let account = Account::from(1);
        let block = builder.genesis().send(account, 100);

        ledger.process_one(&block).unwrap();
        ledger.confirm(block.hash());

        let entry = BacklogEntry {
            hash: block.hash(),
            account,
            ..BacklogEntry::new_test_instance()
        };
        state.index.insert(entry);
        let mut scan = create_test_scan(state, ledger);

        scan.scan_batch();

        let (index, _) = dismantle(scan);
        assert!(index.is_empty());
    }

    /*
     * Test Helpers
     */

    fn create_test_scan(state: BoundedBacklogState, ledger: Ledger) -> RecentlyConfirmedScan {
        let state = Arc::new(NullableCondvarMutex::new(state));
        let stats = Arc::new(BoundedBacklogStats::default());
        RecentlyConfirmedScan::new(state, stats, ledger.into(), TEST_BATCH_SIZE)
    }

    fn dismantle(mut scan: RecentlyConfirmedScan) -> (BacklogIndex, BoundedBacklogStats) {
        let mut index = BacklogIndex::new(1);
        let mut guard = scan.state.lock();
        std::mem::swap(&mut index, &mut guard.index);
        let stats = Arc::get_mut(&mut scan.stats).expect("should have exclusive access to stats");
        let mut stats2 = BoundedBacklogStats::default();
        std::mem::swap(stats, &mut stats2);
        (index, stats2)
    }

    const TEST_BATCH_SIZE: usize = 3;

    fn test_state() -> BoundedBacklogState {
        BoundedBacklogState::new(BoundedBacklogConfig {
            max_backlog: 0,
            batch_size: TEST_BATCH_SIZE,
            scan_rate: 0,
        })
    }
}
