use crate::{
    cementation::ConfirmingSet,
    config::NetworkConstants,
    consensus::{ActiveElectionsContainer, AecInsertRequest},
};
use rsnano_core::{utils::ContainerInfo, Block, BlockHash, PreOrderingBlock, SavedBlock, OrderingBlock, Amount, PublicKey};
use rsnano_ledger::{AnySet, Ledger, RepWeightCache};
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{DetailType, StatType, Stats};

use std::{
    sync::{
        Arc, Condvar, Mutex, RwLock,
    },
    thread::JoinHandle,
    collections::HashMap,
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
    // New fields for preordering block tracking
    preordering_blocks: Mutex<HashMap<BlockHash, (SavedBlock, Amount)>>, // hash -> (block, author_weight)
    total_preordering_weight: Mutex<Amount>,
    quorum_delta: Mutex<Amount>,
    rep_weights: Arc<RepWeightCache>,
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
        rep_weights: Arc<RepWeightCache>,
    ) -> Self {
        let max_elections = active_elections.read().unwrap().max_len() / 10; // 10% of max elections
        let quorum_delta = Amount::nano(67_000_000); // Default quorum delta, will be updated

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
            preordering_blocks: Mutex::new(HashMap::new()),
            total_preordering_weight: Mutex::new(Amount::zero()),
            quorum_delta: Mutex::new(quorum_delta),
            rep_weights,
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

    /// Called when a preordering block is received
    pub fn on_preordering_block_received(&self, preordering_block: SavedBlock) {
        let hash = preordering_block.hash();
        
        // Avoid duplicates
        if self.preordering_blocks.lock().unwrap().contains_key(&hash) {
            println!("DEBUG: Duplicate preordering block received, ignoring: {}", hash);
            return;
        }
        
        // Get the author's voting weight
        let author_account = preordering_block.account_field().unwrap_or_default();
        let author_public_key = PublicKey::from(author_account);
        let author_weight = self.rep_weights.weight(&author_public_key);
        
        println!("DEBUG: Preordering block received from account: {}, weight: {:?}", author_account, author_weight);
        
        // Add the preordering block and its author's voting weight
        self.preordering_blocks.lock().unwrap().insert(hash, (preordering_block, author_weight));
        *self.total_preordering_weight.lock().unwrap() += author_weight;
        
        let total_weight = *self.total_preordering_weight.lock().unwrap();
        let quorum_delta = *self.quorum_delta.lock().unwrap();
        println!("DEBUG: Total preordering weight: {:?}, quorum delta: {:?}", total_weight, quorum_delta);
        
        // Check if we have enough accumulated weight to form an ordering block
        if self.should_create_ordering_block() {
            println!("DEBUG: Quorum reached, creating ordering block");
            self.create_and_insert_ordering_block();
        } else {
            println!("DEBUG: Quorum not reached yet");
        }
    }

    fn should_create_ordering_block(&self) -> bool {
        let total_weight = *self.total_preordering_weight.lock().unwrap();
        let quorum_delta = *self.quorum_delta.lock().unwrap();
        total_weight >= quorum_delta
    }

    fn create_and_insert_ordering_block(&self) {
        println!("DEBUG: Creating ordering block");
        
        // Extract all preordering block hashes
        let preordering_blocks = self.preordering_blocks.lock().unwrap();
        let preordering_hashes: Vec<BlockHash> = preordering_blocks.keys().cloned().collect();
        drop(preordering_blocks);
        
        println!("DEBUG: Preordering block hashes: {:?}", preordering_hashes);
        
        // Get current epoch
        let current_epoch = self.mutex.lock().unwrap().current_epoch;
        println!("DEBUG: Current epoch: {}", current_epoch);
        
        // Create ordering block from the preordering block hashes
        let ordering_block = OrderingBlock::new(current_epoch, preordering_hashes, self.ledger.constants.genesis_account);
        
        // Create saved block
        let saved_block = SavedBlock::new_test_instance_with(Block::Ordering(ordering_block));
        println!("DEBUG: Created ordering block with hash: {}", saved_block.hash());
        
        // Insert into active elections
        self.insert_ordering_block_into_aec(saved_block);
        
        // Clear the preordering blocks for the next epoch
        self.preordering_blocks.lock().unwrap().clear();
        *self.total_preordering_weight.lock().unwrap() = Amount::zero();
        self.mutex.lock().unwrap().current_epoch += 1;
        
        println!("DEBUG: Ordering block creation completed");
    }

    fn insert_ordering_block_into_aec(&self, saved_block: SavedBlock) {
        let hash = saved_block.hash();
        let priority = self.ledger.any().block_priority(&saved_block);
        
        println!("DEBUG: Inserting ordering block into AEC, hash: {}, priority: {:?}", hash, priority);
        
        let now = self.clock.now();
        let mut aec = self.active_elections.write().unwrap();
        
        // Insert as ordering election
        let insert_result = aec.insert(AecInsertRequest::new_ordering(saved_block, priority), now);
        println!("DEBUG: AEC insert result: {:?}", insert_result);
        
        if insert_result.is_ok() {
            aec.transition_active(&hash);
            println!("DEBUG: Ordering block successfully inserted and activated in AEC");
        } else {
            println!("DEBUG: Failed to insert ordering block into AEC: {:?}", insert_result);
        }
    }



    fn run(&self) {
        let mut guard = self.mutex.lock().unwrap();
        while !guard.stopped {
            guard = self
                .condition
                .wait_while(guard, |g| !g.stopped && !g.predicate(self.config.committed_threshold))
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

                    // Create pre_ordering block
                    let pre_ordering_block = PreOrderingBlock::new(epoch, committed_blocks, self.ledger.constants.genesis_account);
                    let block = Block::PreOrdering(pre_ordering_block);
                    let saved_block = SavedBlock::new_test_instance_with(block);

                    // Insert the preordering block into AEC instead of broadcasting
                    self.insert_preordering_block_into_aec(saved_block);

                    // Reset the committed count and increment epoch
                    guard.committed_count = 0;
                    guard.current_epoch += 1;

                    drop(guard);
                } else {
                    drop(guard);
                }
                self.notify();
                guard = self.mutex.lock().unwrap();
            }
        }
    }

    fn insert_preordering_block_into_aec(&self, saved_block: SavedBlock) {
        let hash = saved_block.hash();
        let priority = self.ledger.any().block_priority(&saved_block);
        
        println!("DEBUG: Inserting preordering block into AEC, hash: {}, priority: {:?}", hash, priority);
        
        let now = self.clock.now();
        let mut aec = self.active_elections.write().unwrap();
        
        // Insert as preordering election
        let insert_result = aec.insert(AecInsertRequest::new_pre_ordering(saved_block, priority), now);
        println!("DEBUG: AEC insert result for preordering block: {:?}", insert_result);
        
        if insert_result.is_ok() {
            aec.transition_active(&hash);
            println!("DEBUG: Preordering block successfully inserted and activated in AEC");
        } else {
            println!("DEBUG: Failed to insert preordering block into AEC: {:?}", insert_result);
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        let guard = self.mutex.lock().unwrap();
        let preordering_count = self.preordering_blocks.lock().unwrap().len();
        [
            (
                "committed_blocks",
                guard.committed_blocks.len(),
                std::mem::size_of::<SavedBlock>(),
            ),
            (
                "preordering_blocks",
                preordering_count,
                std::mem::size_of::<SavedBlock>(),
            ),
        ]
        .into()
    }

    /// Get the current committed count for testing purposes
    pub fn committed_count(&self) -> u32 {
        let guard = self.mutex.lock().unwrap();
        guard.committed_count
    }

    /// Update quorum delta from online reps
    pub fn update_quorum_delta(&self, quorum_delta: Amount) {
        *self.quorum_delta.lock().unwrap() = quorum_delta;
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
