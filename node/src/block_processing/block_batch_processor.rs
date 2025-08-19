use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering::Relaxed},
        Arc, Mutex,
    },
};

use strum::{EnumCount, IntoEnumIterator};
use tracing::{debug, warn};

use rsnano_core::utils::{backpressure_channel, BackpressureSender};
use rsnano_ledger::{BlockError, Ledger};
use rsnano_nullable_clock::SteadyClock;
use rsnano_stats::{StatsCollection, StatsSource};

use super::{BlockContext, BlockSource, LedgerEvent, UncheckedBlockReenqueuer, UncheckedMap};
use crate::block_processing::ProcessedResult;

pub(crate) struct BlockBatchProcessor {
    pub ledger: Arc<Ledger>,
    pub unchecked: Arc<Mutex<UncheckedMap>>,
    pub stats: Arc<BlockBatchProcessorStats>,
    pub event_publisher: BackpressureSender<LedgerEvent>,
    pub unchecked_reenqueuer: UncheckedBlockReenqueuer,
    pub clock: Arc<SteadyClock>,
}

impl BlockBatchProcessor {
    #[allow(dead_code)]
    pub fn new_null() -> Self {
        Self {
            ledger: Arc::new(Ledger::new_null()),
            unchecked: Arc::new(Mutex::new(UncheckedMap::default())),
            stats: Arc::new(BlockBatchProcessorStats::default()),
            event_publisher: backpressure_channel(0).0,
            unchecked_reenqueuer: UncheckedBlockReenqueuer::new_null(),
            clock: Arc::new(SteadyClock::new_null()),
        }
    }

    pub(crate) fn process_blocks(&mut self, mut batch: VecDeque<Arc<BlockContext>>) {
        let now = self.clock.now();

        self.roll_back_competitor_blocks(&batch);

        let mut result = self.ledger.process_batch(batch.iter().map(|c| &c.block));

        let processed_result: Vec<_> = result
            .processed
            .iter()
            .zip(&batch)
            .map(|((result, block), ctx)| ProcessedResult {
                block: ctx.block.clone(),
                source: ctx.source,
                status: *result,
                saved_block: block.clone(),
            })
            .collect();

        if !processed_result.is_empty() {
            if let Err(e) = self
                .event_publisher
                .send(LedgerEvent::BlocksProcessed(processed_result))
            {
                warn!("Failed to publish blocks processed event: {e:?}");
            }
        }

        assert_eq!(result.processed.len(), batch.len());
        let mut result: Vec<(Result<(), BlockError>, Arc<BlockContext>)> = result
            .processed
            .drain(..)
            .zip(batch.drain(..))
            .map(|((status, saved_block), block_ctx)| {
                if saved_block.is_some() {
                    *block_ctx.saved_block.lock().unwrap() = saved_block;
                }

                (status, block_ctx)
            })
            .collect();

        // Iterate in reverse order so that when consecutive blocks where processed with
        // gap_previous, that the successful insert of the first block is processed last
        // and the unchecked_map trigger succeeds.
        for (status, block_ctx) in result.iter().rev() {
            match status {
                Ok(()) => {
                    self.stats.progress.fetch_add(1, Relaxed);
                }
                Err(e) => {
                    self.stats.errors[*e as usize].fetch_add(1, Relaxed);
                }
            }

            self.stats.sources[block_ctx.source as usize].fetch_add(1, Relaxed);

            let hash = block_ctx.block.hash();
            let block = &block_ctx.block;

            match status {
                Ok(()) => {
                    self.unchecked_reenqueuer
                        .enqueue_blocks_with_dependency(hash);
                }
                Err(BlockError::GapPrevious) => {
                    self.unchecked
                        .lock()
                        .unwrap()
                        .put(block.previous(), block.clone(), now);
                }
                Err(BlockError::GapSource) => {
                    self.unchecked
                        .lock()
                        .unwrap()
                        .put(block.source_or_link(), block.clone(), now);
                }
                Err(BlockError::GapEpochOpenPending) => {}
                Err(BlockError::Old) => {
                    debug!("Block is old: {}", hash)
                }
                Err(BlockError::Conflict) => {
                    debug!("Block conflict: {}", hash)
                }
                // These are unexpected and indicate erroneous/malicious behavior, log debug info to highlight the issue
                Err(BlockError::BadSignature) => {
                    debug!("Block signature is invalid: {}", hash)
                }
                Err(BlockError::NegativeSpend) => {
                    debug!("Block spends negative amount: {}", hash)
                }
                Err(BlockError::Unreceivable) => {
                    debug!("Block is unreceivable: {}", hash)
                }
                Err(BlockError::Fork) => {
                    debug!("Block is a fork: {}", hash)
                }
                Err(BlockError::OpenedBurnAccount) => {
                    debug!("Block opens burn account: {}", hash)
                }
                Err(BlockError::BalanceMismatch) => {
                    debug!("Block balance mismatch: {}", hash)
                }
                Err(BlockError::RepresentativeMismatch) => {
                    debug!("Block representative mismatch: {}", hash)
                }
                Err(BlockError::BlockPosition) => {
                    debug!("Block is in incorrect position: {}", hash)
                }
                Err(BlockError::InsufficientWork) => {
                    debug!("Block has insufficient work: {}", hash)
                }
            }
        }

        // Set results for futures when not holding the lock
        for (res, context) in result.iter_mut() {
            if let Some(cb) = &context.callback {
                cb(*res);
            }
            context.set_result(*res);
        }
    }

    fn roll_back_competitor_blocks(&self, batch: &VecDeque<Arc<BlockContext>>) {
        let fork_blocks = batch.iter().filter_map(|i| {
            if i.source == BlockSource::Forced {
                Some(&i.block)
            } else {
                None
            }
        });
        self.ledger.roll_back_competitors(fork_blocks, |results| {
            if let Err(e) = self
                .event_publisher
                .send(LedgerEvent::BlocksRolledBack(results))
            {
                warn!("Failed to publish rolled back event: {e:?}");
            }
        });
    }
}

#[derive(Default)]
pub(crate) struct BlockBatchProcessorStats {
    progress: AtomicU64,
    errors: [AtomicU64; BlockError::COUNT],
    sources: [AtomicU64; BlockSource::COUNT],
}

impl StatsSource for BlockBatchProcessorStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "block_processor_result",
            "progress",
            self.progress.load(Relaxed),
        );

        for e in BlockError::iter() {
            result.insert(
                "block_processor_result",
                e.into(),
                self.errors[e as usize].load(Relaxed),
            );
        }

        for s in BlockSource::iter() {
            result.insert(
                "block_processor_source",
                s.into(),
                self.sources[s as usize].load(Relaxed),
            );
        }
    }
}
