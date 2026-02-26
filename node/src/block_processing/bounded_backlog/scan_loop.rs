use std::{sync::Arc, time::Duration};

use rsnano_ledger::{Ledger, LedgerSet};
use rsnano_network::token_bucket::TokenBucket;
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;
use rsnano_utils::{
    CancellationToken,
    stats::{DetailType, StatType, Stats},
};

use crate::block_processing::bounded_backlog::BoundedBacklogState;

pub(crate) struct ScanLoop {
    state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    stats: Arc<Stats>,
    scan_limiter: TokenBucket,
    batch_size: usize,
    last: BlockHash,

    // Infrastructure:
    ledger: Arc<Ledger>,
    clock: Arc<SteadyClock>,
}

impl ScanLoop {
    pub(crate) fn new(
        state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
        stats: Arc<Stats>,
        ledger: Arc<Ledger>,
        clock: Arc<SteadyClock>,
        scan_rate: usize,
        batch_size: usize,
    ) -> Self {
        Self {
            state,
            stats,
            ledger,
            clock,
            scan_limiter: TokenBucket::new(scan_rate),
            batch_size,
            last: BlockHash::ZERO,
        }
    }

    pub(crate) fn run(mut self, cancel_token: CancellationToken) {
        let mut confirmed: Vec<BlockHash> = Vec::with_capacity(self.batch_size);

        while !cancel_token.is_cancelled() {
            self.wait_limiter(&cancel_token);

            if cancel_token.is_cancelled() {
                return;
            }

            self.stats
                .inc(StatType::BoundedBacklog, DetailType::LoopScan);

            let batch = self.state.lock().index.next(&self.last, self.batch_size);

            self.stats.add(
                StatType::BoundedBacklog,
                DetailType::Scanned,
                batch.len() as u64,
            );

            // If batch is empty, we iterated over all accounts in the index
            self.last = batch.last().cloned().unwrap_or_default();

            if !batch.is_empty() {
                confirmed.clear();
                self.check_confirmed(&batch, &mut confirmed);

                if !confirmed.is_empty() {
                    self.state.lock().index.erase_hashes(&confirmed);
                    self.state.notify_all();
                }
            }
        }
    }

    fn wait_limiter(&mut self, cancel_token: &CancellationToken) {
        while !self
            .scan_limiter
            .try_consume(self.batch_size, self.clock.now())
        {
            if cancel_token.wait_for_cancellation(Duration::from_millis(100)) {
                break;
            }
        }
    }

    fn check_confirmed(&self, hashes: &[BlockHash], confirmed: &mut Vec<BlockHash>) {
        let unconfirmed = self.ledger.unconfirmed();
        for hash in hashes {
            // Erase if the block is either confirmed or missing
            if !unconfirmed.block_exists(&hash) {
                confirmed.push(*hash);
            }
        }
    }
}
