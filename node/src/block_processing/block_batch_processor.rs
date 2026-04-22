use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering::Relaxed},
        Arc, Mutex,
    },
};

use strum::{EnumCount, IntoEnumIterator};
use tracing::trace;

use rsnano_ledger::{BlockError, BlockSource, Ledger};
use rsnano_nullable_clock::SteadyClock;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use super::{BlockContext, UncheckedBlockReenqueuer, UncheckedMap};

pub(crate) struct BlockBatchProcessor {
    pub ledger: Arc<Ledger>,
    pub unchecked: Arc<Mutex<UncheckedMap>>,
    pub stats: Arc<BlockBatchProcessorStats>,
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
            unchecked_reenqueuer: UncheckedBlockReenqueuer::new_null(),
            clock: Arc::new(SteadyClock::new_null()),
        }
    }

    pub(crate) fn process_blocks(&mut self, batch: VecDeque<Arc<BlockContext>>) {
        let now = self.clock.now();

        self.roll_back_competitor_blocks(&batch);

        let process_result = self
            .ledger
            .process_batch(batch.iter().map(|c| (&c.block, c.source)));

        assert_eq!(process_result.len(), batch.len());
        let result: Vec<(Result<(), BlockError>, Arc<BlockContext>)> = process_result
            .into_iter()
            .zip(batch.into_iter())
            .map(|(result, block_ctx)| {
                if result.saved_block.is_some() {
                    *block_ctx.saved_block.lock().unwrap() = result.saved_block;
                }

                (result.status, block_ctx)
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
                    self.stats.add_error(e);
                }
            }

            self.stats.sources[block_ctx.source as usize].fetch_add(1, Relaxed);

            let hash = block_ctx.block.hash();
            let block = &block_ctx.block;

            match status {
                Ok(()) => {
                    trace!(block_hash = %hash, "Block processed");
                    self.unchecked_reenqueuer
                        .enqueue_blocks_with_dependency(hash);
                }
                Err(error) => {
                    trace!(block_hash = %hash, ?error, "Block processing failed");
                    // Bootstrap has its own block cache which would cause conflicts with the
                    // unchecked map
                    if block_ctx.source != BlockSource::Bootstrap {
                        match error {
                            BlockError::GapPrevious => {
                                self.unchecked.lock().unwrap().put(
                                    block.previous(),
                                    block.clone(),
                                    now,
                                );
                            }
                            BlockError::GapSource => {
                                self.unchecked.lock().unwrap().put(
                                    block.source_or_link(),
                                    block.clone(),
                                    now,
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Set results for futures when not holding the lock
        for (res, context) in result.into_iter() {
            if let Some(cb) = &context.callback {
                let saved_block = context.saved_block.lock().unwrap().clone();
                (cb)(&context.block.hash(), res.clone(), saved_block.as_ref());
            }
            context.set_result(res);
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
        self.ledger.roll_back_competitors(fork_blocks);
    }
}

#[derive(Default)]
pub(crate) struct BlockBatchProcessorStats {
    progress: AtomicU64,
    bad_signature: AtomicU64,
    old: AtomicU64,
    negative_spend: AtomicU64,
    fork: AtomicU64,
    unreceivable: AtomicU64,
    gap_previous: AtomicU64,
    gap_source: AtomicU64,
    gap_epoch_open: AtomicU64,
    opened_burn_account: AtomicU64,
    balance_mismatch: AtomicU64,
    representative_mismatch: AtomicU64,
    block_position: AtomicU64,
    insufficient_work: AtomicU64,
    conflict: AtomicU64,
    sources: [AtomicU64; BlockSource::COUNT],
}

impl BlockBatchProcessorStats {
    pub fn add_error(&self, error: &BlockError) {
        let counter = match error {
            BlockError::BadSignature => &self.bad_signature,
            BlockError::Old(_) => &self.old,
            BlockError::NegativeSpend => &self.negative_spend,
            BlockError::Fork => &self.fork,
            BlockError::Unreceivable => &self.unreceivable,
            BlockError::GapPrevious => &self.gap_previous,
            BlockError::GapSource => &self.gap_source,
            BlockError::GapEpochOpenPending => &self.gap_epoch_open,
            BlockError::OpenedBurnAccount => &self.opened_burn_account,
            BlockError::BalanceMismatch => &self.balance_mismatch,
            BlockError::RepresentativeMismatch => &self.representative_mismatch,
            BlockError::BlockPosition => &self.block_position,
            BlockError::InsufficientWork => &self.insufficient_work,
            BlockError::Conflict => &self.conflict,
        };
        counter.fetch_add(1, Relaxed);
    }
}

impl StatsSource for BlockBatchProcessorStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const KEY: &'static str = "block_processor_result";

        result.insert(KEY, "progress", self.progress.load(Relaxed));
        result.insert(KEY, "bad_signature", self.bad_signature.load(Relaxed));
        result.insert(KEY, "old", self.old.load(Relaxed));
        result.insert(KEY, "negative_spend", self.negative_spend.load(Relaxed));
        result.insert(KEY, "fork", self.fork.load(Relaxed));
        result.insert(KEY, "unreceivable", self.unreceivable.load(Relaxed));
        result.insert(KEY, "gap_previous", self.gap_previous.load(Relaxed));
        result.insert(KEY, "gap_source", self.gap_source.load(Relaxed));
        result.insert(
            KEY,
            "gap_epoch_open_pending",
            self.gap_epoch_open.load(Relaxed),
        );
        result.insert(
            KEY,
            "opened_burn_account",
            self.opened_burn_account.load(Relaxed),
        );
        result.insert(KEY, "balance_mismatch", self.balance_mismatch.load(Relaxed));
        result.insert(
            KEY,
            "representative_mismatch",
            self.representative_mismatch.load(Relaxed),
        );
        result.insert(KEY, "block_position", self.block_position.load(Relaxed));
        result.insert(
            KEY,
            "insufficient_work",
            self.insufficient_work.load(Relaxed),
        );
        result.insert(KEY, "conflict", self.conflict.load(Relaxed));

        for s in BlockSource::iter() {
            result.insert(
                "block_processor_source",
                s.into(),
                self.sources[s as usize].load(Relaxed),
            );
        }
    }
}
