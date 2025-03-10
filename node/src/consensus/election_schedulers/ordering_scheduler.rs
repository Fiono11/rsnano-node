use rsnano_stats::Stats;
use std::{sync::{atomic::AtomicBool, Arc, Condvar, Mutex}, thread::JoinHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::collections::HashSet;
use rsnano_core::BlockHash;

pub struct OrderingScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    condition: Condvar,
    stats: Arc<Stats>,
    stopped: AtomicBool,
    committed_count: AtomicUsize,
    committed_threshold: usize,
    committed_blocks: Mutex<HashSet<BlockHash>>,
}

impl OrderingScheduler {
    pub fn new(
        stats: Arc<Stats>, 
    ) -> Self {
        Self {
            thread: Mutex::new(None),
            condition: Condvar::new(),
            stopped: AtomicBool::new(true),
            stats,
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
        if count >= self.committed_threshold {
            self.committed_count.store(0, Ordering::SeqCst);
            self.run();
        }
        count
    }

    fn run(&self) {

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