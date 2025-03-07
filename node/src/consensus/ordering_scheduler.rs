use crate::cementation::ConfirmingSet;

use super::{ActiveElections, ElectionBehavior};
use rsnano_core::Era;
use rsnano_stats::Stats;
use std::{sync::{atomic::AtomicBool, Arc, Condvar, Mutex, RwLock}, thread::JoinHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::HashSet;
use rsnano_core::BlockHash;
use rsnano_stats::{DetailType, StatType};

pub struct OrderingScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    condition: Condvar,
    stats: Arc<Stats>,
    active: Arc<ActiveElections>,
    stopped: AtomicBool,
    confirming_set: Arc<ConfirmingSet>,
    committed_count: AtomicUsize,
    committed_threshold: usize,
    committed_blocks: Mutex<HashSet<BlockHash>>,
}

impl OrderingScheduler {
    pub fn new(
        stats: Arc<Stats>, 
        active: Arc<ActiveElections>,
        confirming_set: Arc<ConfirmingSet>,
    ) -> Self {
        Self {
            thread: Mutex::new(None),
            condition: Condvar::new(),
            stopped: AtomicBool::new(true),
            stats,
            active,
            confirming_set,
            committed_count: AtomicUsize::new(0),
            committed_threshold: 1000, // Configurable threshold
            committed_blocks: Mutex::new(HashSet::new()),
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify();
        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
    }

    /// Notify about changes in AEC vacancy
    pub fn notify(&self) {
        self.condition.notify_all();
    }

    pub fn increment_committed_count(&self, block_hash: BlockHash) -> usize {
        let mut blocks = self.committed_blocks.lock().unwrap();
        blocks.insert(block_hash);
        
        let count = self.committed_count.fetch_add(1, Ordering::SeqCst) + 1;
        if count >= self.committed_threshold {
            self.committed_count.store(0, Ordering::SeqCst);
            self.run();
        }
        count
    }

    fn run(&self) {
        let blocks_to_order: HashSet<BlockHash> = {
            let blocks = self.committed_blocks.lock().unwrap();
            blocks.clone()
        };
        
        {
            let mut blocks = self.committed_blocks.lock().unwrap();
            blocks.clear();
        }

        let result = self
            .active
            .insert_ordering(blocks_to_order, ElectionBehavior::Ordering, None);
        
        //self.stats.inc(StatType::OrderingScheduler, DetailType::ScheduleOrdering);
        
        // TODO: Create the actual ordering election with the blocks_to_order
        // This would involve creating a special block that references all the blocks to be ordered
    }
}

impl Drop for OrderingScheduler {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        debug_assert!(self.thread.lock().unwrap().is_none());
    }
}