use rsnano_core::{Block, BlockHash, OrderBlock, OrderBlockArgs};
use rsnano_ledger::Ledger;
use rsnano_stats::{DetailType, StatType, Stats};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{
    sync::{atomic::AtomicBool, Arc, Condvar, Mutex},
    thread::JoinHandle,
};

use crate::consensus::{ActiveElections, ElectionBehavior};

pub struct OrderingScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    condition: Condvar,
    stats: Arc<Stats>,
    stopped: AtomicBool,
    active: Arc<ActiveElections>,
    ledger: Arc<Ledger>,
    committed_count: AtomicUsize,
    committed_threshold: usize,
    committed_blocks: Mutex<HashSet<BlockHash>>,
}

impl OrderingScheduler {
    pub fn new(stats: Arc<Stats>, active: Arc<ActiveElections>, ledger: Arc<Ledger>) -> Self {
        Self {
            thread: Mutex::new(None),
            condition: Condvar::new(),
            stopped: AtomicBool::new(true),
            stats,
            active,
            ledger,
            committed_count: AtomicUsize::new(0),
            committed_threshold: 1000,
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

    pub fn notify(&self) {
        self.condition.notify_all();
    }

    pub fn increment_committed_count(&self, block_hash: BlockHash) -> usize {
        let mut blocks = self.committed_blocks.lock().unwrap();
        blocks.insert(block_hash);

        let count = self.committed_count.fetch_add(1, Ordering::SeqCst) + 1;
        if self.predicate() {
            self.notify();
        }
        count
    }

    fn predicate(&self) -> bool {
        let count = self.committed_count.load(Ordering::SeqCst);
        count >= self.committed_threshold
    }

    fn run(&self) {
        while !self.stopped.load(Ordering::SeqCst) {
            let mut guard = self.condition.wait_while(
                self.committed_blocks.lock().unwrap(),
                |_| !self.predicate() && !self.stopped.load(Ordering::SeqCst),
            ).unwrap();

            if self.stopped.load(Ordering::SeqCst) {
                break;
            }

            if self.predicate() {
                self.committed_count.store(0, Ordering::SeqCst);
                
                let blocks_to_order: HashSet<BlockHash> = guard.clone();
                guard.clear();
                
                drop(guard);

                let mut args = OrderBlockArgs::new_test_instance();
                args.previous = BlockHash::zero();

                let order_block: Block = args.into();
                let saved_block = self.ledger.process_one(&order_block).unwrap();

                let result = self
                    .active
                    .insert(saved_block, ElectionBehavior::Ordering, None);

                let inserted = result.map(|i| i.inserted).unwrap_or(false);
                if inserted {
                    self.stats
                        .inc(StatType::ElectionScheduler, DetailType::Insert);
                } else {
                    self.stats
                        .inc(StatType::ElectionScheduler, DetailType::InsertFailed);
                }
            }
        }
    }
}

impl Drop for OrderingScheduler {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        debug_assert!(self.thread.lock().unwrap().is_none());
    }
}

pub trait OrderingSchedulerExt {
    fn start(&self);
}

impl OrderingSchedulerExt for Arc<OrderingScheduler> {
    fn start(&self) {
        debug_assert!(self.thread.lock().unwrap().is_none());
        self.stopped.store(false, Ordering::SeqCst);
        let self_l = Arc::clone(self);
        *self.thread.lock().unwrap() = Some(
            std::thread::Builder::new()
                .name("Sched Ord".to_string())
                .spawn(Box::new(move || {
                    self_l.run();
                }))
                .unwrap(),
        );
    }
}
