use crate::cementation::ConfirmingSet;

use super::{ActiveElections, ElectionBehavior};
use rsnano_core::Era;
use rsnano_stats::Stats;
use std::{sync::{Arc, Condvar, Mutex}, thread::JoinHandle};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct OrderingScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    condition: Condvar,
    mutex: Mutex<OrderingSchedulerImpl>,
    stats: Arc<Stats>,
    active: Arc<ActiveElections>,
    confirming_set: Arc<ConfirmingSet>,
    committed_count: AtomicUsize,
    committed_threshold: usize,
}

#[derive(Debug)]
struct OrderingSchedulerImpl {
    current_era: Option<Era>,
    stopped: bool,
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
            mutex: Mutex::new(OrderingSchedulerImpl {
                current_era: None,
                stopped: false,
            }),
            stats,
            active,
            confirming_set,
            committed_count: AtomicUsize::new(0),
            committed_threshold: 1000, // Configurable threshold
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

    pub fn increment_committed_count(&self) -> usize {
        let count = self.committed_count.fetch_add(1, Ordering::SeqCst) + 1;
        if count >= self.committed_threshold {
            self.committed_count.store(0, Ordering::SeqCst);
            self.schedule_ordering_election();
        }
        count
    }

    fn schedule_ordering_election(&self) {
        // Implementation to start a new ordering election
    }
}