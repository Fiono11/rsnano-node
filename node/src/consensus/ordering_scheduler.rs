use crate::cementation::ConfirmingSet;

use super::{ActiveElections, ElectionBehavior, OrderingElection};
use rsnano_core::Era;
use rsnano_stats::Stats;
use std::{sync::{Arc, Condvar, Mutex, RwLock}, thread::JoinHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::HashSet;
use rsnano_core::BlockHash;
use rsnano_stats::{DetailType, StatType};

pub struct OrderingScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    condition: Condvar,
    mutex: Mutex<OrderingSchedulerImpl>,
    stats: Arc<Stats>,
    active: Arc<ActiveElections>,
    confirming_set: Arc<ConfirmingSet>,
    committed_count: AtomicUsize,
    committed_threshold: usize,
    committed_blocks: RwLock<HashSet<BlockHash>>,
}

struct OrderingSchedulerImpl {
    current_era: Option<Era>,
    stopped: bool,
    current_ordering_election: Option<Arc<Mutex<OrderingElection>>>,
}

impl Default for OrderingSchedulerImpl {
    fn default() -> Self {
        Self {
            current_era: None,
            stopped: false,
            current_ordering_election: None,
        }
    }
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
            mutex: Mutex::new(OrderingSchedulerImpl::default()),
            stats,
            active,
            confirming_set,
            committed_count: AtomicUsize::new(0),
            committed_threshold: 1000, // Configurable threshold
            committed_blocks: RwLock::new(HashSet::new()),
        }
    }

    pub fn stop(&self) {
        let mut guard = self.mutex.lock().unwrap();
        guard.stopped = true;
        drop(guard);
        self.condition.notify_all();
        if let Some(thread) = self.thread.lock().unwrap().take() {
            thread.join().unwrap();
        }
    }

    pub fn increment_committed_count(&self, block_hash: BlockHash) -> usize {
        let mut blocks = self.committed_blocks.write().unwrap();
        blocks.insert(block_hash);
        
        let count = self.committed_count.fetch_add(1, Ordering::SeqCst) + 1;
        if count >= self.committed_threshold {
            self.committed_count.store(0, Ordering::SeqCst);
            self.schedule_ordering_election();
        }
        count
    }

    fn schedule_ordering_election(&self) {
        let blocks_to_order = {
            let blocks = self.committed_blocks.read().unwrap();
            blocks.clone()
        };
        
        {
            let mut blocks = self.committed_blocks.write().unwrap();
            blocks.clear();
        }
        
        //self.stats.inc(StatType::OrderingScheduler, DetailType::ScheduleOrdering);
        
        // TODO: Create the actual ordering election with the blocks_to_order
        // This would involve creating a special block that references all the blocks to be ordered
    }
}


