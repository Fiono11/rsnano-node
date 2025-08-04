use crate::{
    cementation::ConfirmingSet,
    config::NetworkConstants,
    consensus::{ActiveElectionsContainer, AecInsertRequest},
};
use rsnano_core::{utils::ContainerInfo, Block, BlockHash, OrderingBlock, SavedBlock};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{DetailType, StatType, Stats};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, RwLock,
    },
    thread::JoinHandle,
};

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
    condition: Condvar,
    mutex: Mutex<OrderingSchedulerImpl>,
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
            condition: Condvar::new(),
            mutex: Mutex::new(OrderingSchedulerImpl {
                committed_blocks: Vec::new(),
                committed_count: 0,
                current_epoch: 1,
                stopped: false,
            }),
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
        {
            let mut guard = self.mutex.lock().unwrap();
            guard.stopped = true;
        }
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
        let mut guard = self.mutex.lock().unwrap();
        guard.committed_count += confirmed_count as u32;

        if guard.committed_count >= self.config.committed_threshold {
            self.notify();
        }
    }

    /// Called when blocks are confirmed to track the actual committed blocks
    pub fn on_blocks_confirmed_with_hashes(&self, confirmed_blocks: &[rsnano_core::BlockHash]) {
        let mut guard = self.mutex.lock().unwrap();
        // Store the block hashes for later processing
        for hash in confirmed_blocks {
            guard.committed_blocks.push(*hash);
        }
    }

    fn run(&self) {
        let mut guard = self.mutex.lock().unwrap();
        while !guard.stopped {
            guard = self
                .condition
                .wait_while(guard, |g| {
                    !g.stopped && !g.predicate(self.config.committed_threshold)
                })
                .unwrap();

            if !guard.stopped {
                self.stats
                    .inc(StatType::ElectionScheduler, DetailType::Loop);

                if guard.predicate(self.config.committed_threshold) {
                    // Get current epoch and committed blocks
                    let epoch = guard.current_epoch;
                    let committed_blocks = guard.committed_blocks.clone();

                    // Clear the committed blocks for next batch
                    guard.committed_blocks.clear();

                    // Create ordering block
                    let ordering_block = OrderingBlock::new(epoch, committed_blocks);
                    let block = Block::Ordering(ordering_block);
                    let saved_block = SavedBlock::new_test_instance_with(block);

                    let hash = saved_block.hash();
                    let priority = self.ledger.any().block_priority(&saved_block);
                    self.stats
                        .inc(StatType::ElectionScheduler, DetailType::InsertOrdering);

                    let now = self.clock.now();
                    let mut aec = self.active_elections.write().unwrap();

                    if aec
                        .insert(AecInsertRequest::new_ordering(saved_block, priority), now)
                        .is_ok()
                    {
                        aec.transition_active(&hash);
                    }

                    // Reset the committed count and increment epoch
                    guard.committed_count = 0;
                    guard.current_epoch += 1;
                }

                guard = self.mutex.lock().unwrap();
            }
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        let guard = self.mutex.lock().unwrap();
        [(
            "committed_blocks",
            guard.committed_blocks.len(),
            std::mem::size_of::<SavedBlock>(),
        )]
        .into()
    }

    /// Get the current committed count for testing purposes
    pub fn committed_count(&self) -> u32 {
        let guard = self.mutex.lock().unwrap();
        guard.committed_count
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

struct OrderingSchedulerImpl {
    committed_blocks: Vec<BlockHash>,
    committed_count: u32,
    current_epoch: u64,
    stopped: bool,
}

impl OrderingSchedulerImpl {
    fn predicate(&self, threshold: u32) -> bool {
        self.committed_count >= threshold
    }
}
