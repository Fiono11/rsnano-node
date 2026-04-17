use std::sync::{Arc, Mutex};

use rsnano_ledger::{AnySet, BlockError, BlockSource, Ledger, ProcessResult};
use rsnano_network::ChannelId;
use rsnano_types::{Account, Block, BlockType, SavedBlock};

use super::logic::BootstrapLogic;
use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::logic::Priority,
};

/// Inspects a processed block and adjusts the bootstrap state accordingly
pub(super) struct BlockInspector {
    state: Arc<Mutex<BootstrapLogic>>,
    ledger: Arc<Ledger>,
    block_processor_queue: Arc<BlockProcessorQueue>,
}

impl BlockInspector {
    pub(super) fn new(
        state: Arc<Mutex<BootstrapLogic>>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
    ) -> Self {
        Self {
            state,
            ledger,
            block_processor_queue,
        }
    }

    pub fn inspect(&self, batch: &[ProcessResult]) {
        let mut state = self.state.lock().unwrap();
        let any = self.ledger.any();
        for result in batch {
            let account = self.get_account(&any, &result.block, &result.saved_block);
            self.inspect_block(&mut state, result, &account);
        }
        self.enqueue_next_blocks(&mut state);
    }

    fn enqueue_next_blocks(&self, state: &mut BootstrapLogic) {
        while let Some(block) = state.bootstrap_queue.next_block_to_process() {
            let block_hash = block.hash();

            let inserted = self.block_processor_queue.push(BlockContext::new(
                block.clone(),
                BlockSource::Bootstrap,
                // TODO use real channel id
                ChannelId::LOOPBACK,
            ));

            if inserted {
                state.bootstrap_queue.processing_started(&block_hash);
            } else {
                // block processor queue is full!
                break;
            }
        }
    }

    fn get_account(
        &self,
        any: &dyn AnySet,
        block: &Block,
        saved_block: &Option<SavedBlock>,
    ) -> Account {
        match saved_block {
            Some(b) => b.account(),
            None => block
                .account_field()
                .unwrap_or_else(|| any.block_account(&block.previous()).unwrap_or_default()),
        }
    }

    /// Inspects a block that has been processed by the block processor
    /// - Marks an account as blocked if the result code is gap source as there is no reason request additional blocks for this account until the dependency is resolved
    /// - Marks an account as forwarded if it has been recently referenced by a block that has been inserted.
    fn inspect_block(&self, state: &mut BootstrapLogic, result: &ProcessResult, account: &Account) {
        let hash = result.block.hash();

        match &result.status {
            Ok(()) => {
                state.bootstrap_queue.processing_finished(&hash);

                let saved_block = result.saved_block.as_ref().unwrap();
                let account = saved_block.account();
                // If we've inserted any block in to an account, unmark it as blocked
                state.bootstrap_queue.unblock(account);

                // Progress blocks from live traffic don't need further bootstrapping
                if result.source == BlockSource::Bootstrap {
                    state.bootstrap_queue.priority_up(&account);
                }

                if saved_block.is_send() {
                    let destination = saved_block.destination().unwrap();
                    if !destination.is_zero() {
                        state.bootstrap_queue.unblock(destination);
                        state
                            .bootstrap_queue
                            .priority_up_to(&destination, Priority::INITIAL);
                    }
                }
            }
            Err(error) => {
                match error {
                    BlockError::Old(_) => {
                        state.bootstrap_queue.processing_finished(&hash);
                    }
                    BlockError::GapSource => {
                        let source = result.block.source_or_link();

                        if !account.is_zero() && !source.is_zero() {
                            // Mark account as blocked because it is missing the source block
                            state.bootstrap_queue.block(*account, source);
                        }
                    }
                    BlockError::GapPrevious => {
                        state.bootstrap_queue.remove(&account);
                        // Prevent live traffic from evicting accounts from the priority list
                        if result.source == BlockSource::Live
                            && !state.bootstrap_queue.queue_half_full()
                            && !state.bootstrap_queue.blocked_half_full()
                            && result.block.block_type() == BlockType::State
                        {
                            let dep_account = result.block.account_field().unwrap();
                            if !dep_account.is_zero() {
                                state
                                    .bootstrap_queue
                                    .priority_up_to(&dep_account, Priority::INITIAL);
                            }
                        }
                    }
                    BlockError::GapEpochOpenPending => {
                        // Epoch open blocks for accounts that don't have any pending blocks yet
                        state.bootstrap_queue.remove(account);
                    }
                    BlockError::Conflict => {
                        state.bootstrap_queue.reprocess(account, &hash);
                        // can happen if the unchecked map inserts at the same time
                    }
                    _ => {
                        state.bootstrap_queue.remove(account);
                        // No need to handle other cases
                        // TODO: If we receive blocks that are invalid (bad signature, fork, etc.),
                        // we should penalize the peer that sent them
                    }
                }
            }
        }
    }
}
