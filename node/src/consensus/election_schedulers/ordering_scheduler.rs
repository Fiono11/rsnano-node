use crate::{cementation::ConfirmingSet, config::NetworkConstants, consensus::ActiveElectionsContainer};
use rsnano_core::utils::ContainerInfo;
use rsnano_ledger::Ledger;
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{DetailType, StatType, Stats};
use std::{sync::{atomic::{AtomicBool, Ordering}, Arc, Condvar, Mutex, RwLock}, thread::JoinHandle};

#[derive(Clone, Debug, PartialEq)]
pub struct OrderingSchedulerConfig {
    pub committed_threshold: u32,
}

impl OrderingSchedulerConfig {
    pub fn new() -> Self {
        Self {
            committed_threshold: 1000,
        }
    }
}

impl Default for OrderingSchedulerConfig {
    fn default() -> Self {
        Self::new()
    }
}

pub struct OrderingScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    config: OrderingSchedulerConfig,
    stopped: AtomicBool,
    condition: Condvar,
    committed_count: Mutex<u32>,
    stats: Arc<Stats>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    network_constants: NetworkConstants,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    clock: Arc<SteadyClock>,
    pub max_elections: usize,
}

impl OrderingScheduler {
    pub fn new(
        config: OrderingSchedulerConfig,
        stats: Arc<Stats>,
        active_elections: Arc<RwLock<ActiveElectionsContainer>>,
        network_constants: NetworkConstants,
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        let max_elections = active_elections.read().unwrap().max_len() / 10; // 10% of max elections

        Self {
            thread: Mutex::new(None),
            config,
            stopped: AtomicBool::new(true),
            condition: Condvar::new(),
            committed_count: Mutex::new(0),
            stats,
            active_elections,
            network_constants,
            ledger,
            confirming_set,
            clock,
            max_elections,
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

    /// Notify about changes in committed blocks
    pub fn notify(&self) {
        self.condition.notify_all();
    }

    /// Called when blocks are confirmed to track committed count
    pub fn on_blocks_confirmed(&self, confirmed_count: usize) {
        let mut count = self.committed_count.lock().unwrap();
        *count += confirmed_count as u32;
        
        if *count >= self.config.committed_threshold {
            self.notify();
        }
    }

    fn predicate(&self) -> bool {
        let count = self.committed_count.lock().unwrap();
        *count >= self.config.committed_threshold
    }

    fn run(&self) {
        let mut guard = self.committed_count.lock().unwrap();
        while !self.stopped.load(Ordering::SeqCst) {
            self.stats
                .inc(StatType::OrderingScheduler, DetailType::Loop);

            if self.predicate() {
                *guard = 0;
                drop(guard);
            } else {
                drop(guard);
            }
            self.notify();
            guard = self.committed_count.lock().unwrap();
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        let committed_count = self.committed_count.lock().unwrap();
        [(
            "committed_count",
            *committed_count as usize,
            std::mem::size_of::<u32>(),
        )]
        .into()
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