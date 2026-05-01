use std::sync::Arc;

use rsnano_ledger::{AnySet, BlockError, BlockSource, Ledger, ProcessResult};
use rsnano_network::ChannelId;
use rsnano_types::{Account, Block, BlockType, SavedBlock};

use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::{Priority, bootstrap_queue::BootstrapQueue},
};

/// Inspects a processed block and adjusts the bootstrap state accordingly
pub(super) struct BlockInspector {
    bootstrap_queue: Arc<BootstrapQueue>,
    ledger: Arc<Ledger>,
    block_processor_queue: Arc<BlockProcessorQueue>,
}

impl BlockInspector {
    pub(super) fn new(
        bootstrap_queue: Arc<BootstrapQueue>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
    ) -> Self {
        Self {
            bootstrap_queue,
            ledger,
            block_processor_queue,
        }
    }

    pub fn inspect(&self, batch: &[ProcessResult]) {
        let any = self.ledger.any();
        for result in batch {
            let account = self.get_account(&any, &result.block, &result.saved_block);
            self.inspect_block(result, &account, &any);
        }
        self.enqueue_next_blocks();
    }

    fn enqueue_next_blocks(&self) {
        while let Some(block) = self.bootstrap_queue.take_next_block_for_processing() {
            let block_hash = block.hash();

            let inserted = self.block_processor_queue.push(BlockContext::new(
                block,
                BlockSource::Bootstrap,
                // TODO use real channel id
                ChannelId::LOOPBACK,
            ));

            if !inserted {
                // block processor queue is full — undo the hand-off so the block
                // stays in ready_to_process and can be retried later.
                self.bootstrap_queue.revert_processing_started(&block_hash);
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
    fn inspect_block(&self, result: &ProcessResult, account: &Account, any: &dyn AnySet) {
        let hash = result.block.hash();

        match &result.status {
            Ok(()) => {
                let source = result.source;
                let saved_block = result.saved_block.as_ref().unwrap();
                self.bootstrap_queue
                    .processing_finished(&saved_block.hash());

                let account = saved_block.account();
                // If we've inserted any block in to an account, unmark it as blocked
                self.bootstrap_queue.unblock(account);

                // Progress blocks from live traffic don't need further bootstrapping
                if source == BlockSource::Bootstrap {
                    self.bootstrap_queue.priority_up(&account);
                }

                if saved_block.is_send() {
                    let destination = saved_block.destination().unwrap();
                    if !destination.is_zero() {
                        self.bootstrap_queue.unblock(destination);
                        self.bootstrap_queue
                            .priority_up_to(&destination, Priority::INITIAL);
                    }
                }
            }
            Err(error) => {
                match error {
                    BlockError::Old(_) => {
                        // Can happen due to pull type "safe" which will redownload blocks that we
                        // have already processed
                        self.bootstrap_queue.processing_finished(&hash);
                    }
                    BlockError::GapSource => {
                        let source = result.block.source_or_link();

                        if !account.is_zero() && !source.is_zero() {
                            // We have to recheck the source block, because the parallel block
                            // processing can cause race a condition where the send block got
                            // already processed!
                            if !any.block_exists(&source) {
                                // Mark account as blocked because it is missing the source block
                                self.bootstrap_queue.block(*account, source);
                            } else {
                                // Reprocessing should succeed, because the source block was found
                                self.bootstrap_queue.reprocess(account, &hash);
                            }
                        } else {
                            // The block can't be acted on (account couldn't be resolved or the
                            // source field is zero). Free the processing slot so it doesn't leak.
                            self.bootstrap_queue.processing_finished(&hash);
                        }
                    }
                    BlockError::GapPrevious => {
                        self.bootstrap_queue.remove(&account);
                        // Prevent live traffic from evicting accounts from the priority list
                        if result.source == BlockSource::Live
                            && !self.bootstrap_queue.queue_half_full()
                            && !self.bootstrap_queue.blocked_half_full()
                            && result.block.block_type() == BlockType::State
                        {
                            let dep_account = result.block.account_field().unwrap();
                            if !dep_account.is_zero() {
                                self.bootstrap_queue
                                    .priority_up_to(&dep_account, Priority::INITIAL);
                            }
                        }
                    }
                    BlockError::GapEpochOpenPending => {
                        // Epoch open blocks for accounts that don't have any pending blocks yet
                        self.bootstrap_queue.remove(account);
                    }
                    BlockError::Conflict => self.bootstrap_queue.reprocess(account, &hash),
                    _ => {
                        self.bootstrap_queue.remove(account);
                        // TODO: If we receive blocks that are invalid (bad signature, fork, etc.),
                        // we should penalize the peer that sent them
                    }
                }
            }
        }
    }
}
