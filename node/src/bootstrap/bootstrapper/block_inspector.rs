use std::sync::{Arc, Mutex};

use rsnano_ledger::{AnySet, BlockError, BlockSource, Ledger, ProcessResult};
use rsnano_network::ChannelId;
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Account, Block, BlockType, SavedBlock};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::state::{BootstrapLogic, PriorityUpResult};
use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::state::Priority,
};

/// Inspects a processed block and adjusts the bootstrap state accordingly
pub(super) struct BlockInspector {
    state: Arc<Mutex<BootstrapLogic>>,
    ledger: Arc<Ledger>,
    stats: Arc<Stats>,
    clock: Arc<SteadyClock>,
    block_processor_queue: Arc<BlockProcessorQueue>,
}

impl BlockInspector {
    pub(super) fn new(
        state: Arc<Mutex<BootstrapLogic>>,
        ledger: Arc<Ledger>,
        stats: Arc<Stats>,
        clock: Arc<SteadyClock>,
        block_processor_queue: Arc<BlockProcessorQueue>,
    ) -> Self {
        Self {
            state,
            ledger,
            stats,
            clock,
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
                // TODO delete this:
                state
                    .block_ack_processor
                    .block_queue
                    .enqueued_for_processing(&block_hash);

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

        match result.status {
            Ok(()) => {
                let saved_block = result.saved_block.as_ref().unwrap();
                let account = saved_block.account();
                // If we've inserted any block in to an account, unmark it as blocked
                if state.bootstrap_queue.unblock(account, None) {
                    self.stats
                        .inc(StatType::BootstrapAccountSets, DetailType::Unblock);
                    self.stats.inc(
                        StatType::BootstrapAccountSets,
                        DetailType::PriorityUnblocked,
                    );
                }

                // Progress blocks from live traffic don't need further bootstrapping
                if result.source == BlockSource::Bootstrap {
                    match state.bootstrap_queue.priority_up(&account) {
                        PriorityUpResult::Updated => {
                            self.stats
                                .inc(StatType::BootstrapAccountSets, DetailType::Prioritize);
                        }
                        PriorityUpResult::Inserted => {
                            self.stats
                                .inc(StatType::BootstrapAccountSets, DetailType::Prioritize);
                            self.stats
                                .inc(StatType::BootstrapAccountSets, DetailType::PriorityInsert);
                        }
                        PriorityUpResult::AccountBlocked => {
                            self.stats
                                .inc(StatType::BootstrapAccountSets, DetailType::PrioritizeFailed);
                        }
                    }
                }

                if saved_block.is_send() {
                    let destination = saved_block.destination().unwrap();
                    // Unblocking automatically inserts account into priority set
                    if state.bootstrap_queue.unblock(destination, Some(hash)) {
                        self.stats
                            .inc(StatType::BootstrapAccountSets, DetailType::Unblock);
                        self.stats.inc(
                            StatType::BootstrapAccountSets,
                            DetailType::PriorityUnblocked,
                        );
                    } else if matches!(
                        state.bootstrap_queue.priority_up(&destination),
                        PriorityUpResult::Inserted | PriorityUpResult::Updated
                    ) {
                        self.stats
                            .inc(StatType::BootstrapAccountSets, DetailType::PriorityInsert);
                    } else {
                        self.stats
                            .inc(StatType::BootstrapAccountSets, DetailType::PrioritizeFailed);
                    }
                }

                // TODO delete this
                let info = state.block_ack_processor.block_queue.processed(&hash);
                state.bootstrap_queue.processing_finished(&hash);
                if let Some(account) = info.account
                    && info.was_last
                {
                    state.bootstrap_queue.reset_last_request(&account);
                }
            }
            Err(error) => {
                state
                    .block_ack_processor
                    .block_queue
                    .processing_failed(&hash);

                match error {
                    BlockError::GapSource => {
                        // Prevent malicious live traffic from filling up the blocked set
                        if result.source == BlockSource::Bootstrap {
                            let source = result.block.source_or_link();

                            if !account.is_zero() && !source.is_zero() {
                                // Mark account as blocked because it is missing the source block
                                let blocked =
                                    state
                                        .bootstrap_queue
                                        .block(*account, source, self.clock.now());
                                if blocked {
                                    self.stats.inc(
                                        StatType::BootstrapAccountSets,
                                        DetailType::PriorityEraseBlock,
                                    );
                                    self.stats
                                        .inc(StatType::BootstrapAccountSets, DetailType::Block);
                                } else {
                                    self.stats.inc(
                                        StatType::BootstrapAccountSets,
                                        DetailType::BlockFailed,
                                    );
                                }
                            }
                        }
                    }
                    BlockError::GapPrevious => {
                        // Prevent live traffic from evicting accounts from the priority list
                        if result.source == BlockSource::Live
                            && !state.bootstrap_queue.queue_half_full()
                            && !state.bootstrap_queue.blocked_half_full()
                            && result.block.block_type() == BlockType::State
                        {
                            let account = result.block.account_field().unwrap();
                            if state
                                .bootstrap_queue
                                .priority_set(&account, Priority::INITIAL)
                            {
                                self.stats.inc(
                                    StatType::BootstrapAccountSets,
                                    DetailType::PriorityInsert,
                                );
                            } else {
                                self.stats.inc(
                                    StatType::BootstrapAccountSets,
                                    DetailType::PrioritizeFailed,
                                );
                            }
                        }
                    }
                    BlockError::GapEpochOpenPending => {
                        // Epoch open blocks for accounts that don't have any pending blocks yet
                        if state.bootstrap_queue.remove(account) {
                            self.stats
                                .inc(StatType::BootstrapAccountSets, DetailType::PriorityErase);
                        }
                    }
                    _ => {
                        // No need to handle other cases
                        // TODO: If we receive blocks that are invalid (bad signature, fork, etc.),
                        // we should penalize the peer that sent them
                    }
                }
            }
        }
    }
}
