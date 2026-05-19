use std::sync::Arc;

use rsnano_ledger::{AnySet, BlockError, BlockSource, Ledger, ProcessResult};
use rsnano_network::ChannelId;
use rsnano_types::{BlockType, SavedBlock};

use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::bootstrap_queue::BootstrapQueue,
};

/// Inspects a processed block and adjusts the bootstrap state accordingly
pub(super) struct BlockInspector {
    bootstrap_queue: Arc<BootstrapQueue>,
    ledger: Arc<Ledger>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    pub inspect_live_traffic: bool,
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
            inspect_live_traffic: true,
        }
    }

    pub fn inspect(&self, batch: &[ProcessResult]) {
        let any = self.ledger.any();
        for result in batch {
            self.inspect_block(result, &any);
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

    /// Inspects a block that has been processed by the block processor
    /// - Marks an account as blocked if the result code is gap source as there is no reason request additional blocks for this account until the dependency is resolved
    /// - Marks an account as forwarded if it has been recently referenced by a block that has been inserted.
    fn inspect_block(&self, result: &ProcessResult, any: &dyn AnySet) {
        let hash = result.block.hash();

        match &result.status {
            Ok(()) => {
                let source = result.source;
                let saved_block = result.saved_block.as_ref().unwrap();
                self.handle_processed_block(saved_block, source);
            }
            Err(error) => {
                match error {
                    BlockError::Old(existing_block) => {
                        // Can happen due to pull type "safe" which will redownload blocks that we
                        // have already processed.
                        // It can also happen due duplicate blocks in the block processor queue and
                        // the parallel block processing can cause the old error before we see the
                        // success case...
                        self.handle_processed_block(existing_block, result.source);
                    }
                    BlockError::GapSource => {
                        let source = result.block.source_or_link();

                        if !source.is_zero() {
                            // We have to recheck the source block, because the parallel block
                            // processing can cause race a condition where the send block got
                            // already processed!
                            if !any.block_exists(&source) {
                                // Mark account as blocked because it is missing the source block
                                self.bootstrap_queue.block(&hash, source);
                            } else {
                                // Reprocessing should succeed, because the source block was found
                                self.bootstrap_queue.reprocess(&hash);
                            }
                        } else {
                            self.bootstrap_queue.processing_failed(&hash);
                        }
                    }
                    BlockError::GapPrevious => {
                        self.bootstrap_queue.processing_failed(&hash);
                        if self.inspect_live_traffic {
                            // Prevent live traffic from evicting accounts from the priority list
                            if result.source == BlockSource::Live
                                && !self.bootstrap_queue.queue_half_full()
                                && !self.bootstrap_queue.blocked_half_full()
                                && result.block.block_type() == BlockType::State
                            {
                                let dep_account = result.block.account_field().unwrap();
                                if !dep_account.is_zero() {
                                    self.bootstrap_queue.insert(dep_account);
                                }
                            }
                        }
                    }
                    BlockError::GapEpochOpenPending => {
                        // Epoch open blocks for accounts that don't have any pending blocks yet
                        self.bootstrap_queue.processing_failed(&hash);
                    }
                    BlockError::Conflict => self.bootstrap_queue.reprocess(&hash),
                    _ => {
                        self.bootstrap_queue.processing_failed(&hash);
                        // TODO: If we receive blocks that are invalid (bad signature, fork, etc.),
                        // we should penalize the peer that sent them
                    }
                }
            }
        }
    }

    fn handle_processed_block(&self, saved_block: &SavedBlock, source: BlockSource) {
        self.bootstrap_queue
            .processing_finished(&saved_block.hash());

        if let Some(destination) = saved_block.destination()
            && !destination.is_zero()
        {
            self.bootstrap_queue.unblock_hash(&saved_block.hash());
            if matches!(source, BlockSource::Bootstrap | BlockSource::Unchecked) {
                self.bootstrap_queue.insert(destination);
            }
        }
    }
}
