use std::{sync::Arc, time::Duration};

use rsnano_ledger::{Ledger, LedgerSet};
use rsnano_network::token_bucket::TokenBucket;
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;
use rsnano_utils::stats::{DetailType, StatType, Stats};

use crate::block_processing::bounded_backlog::BoundedBacklogState;

pub(crate) struct ScanLoop {
    state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    stats: Arc<Stats>,
    scan_limiter: TokenBucket,
    batch_size: usize,
    last: BlockHash,
    to_erase: Vec<BlockHash>,

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
            to_erase: Vec::with_capacity(batch_size),
        }
    }

    pub(crate) fn run(mut self) {
        let mut state = self.state.lock();
        while !state.stopped {
            while !self
                .scan_limiter
                .try_consume(self.batch_size, self.clock.now())
            {
                state = self.state.wait_timeout(state, Duration::from_millis(100)).0;
                if state.stopped {
                    return;
                }
            }

            if state.stopped {
                return;
            }

            self.stats
                .inc(StatType::BoundedBacklog, DetailType::LoopScan);

            let batch = state.index.next(&self.last, self.batch_size);
            // If batch is empty, we iterated over all accounts in the index
            if batch.is_empty() {
                self.last = BlockHash::ZERO;
                continue;
            }

            drop(state);
            {
                {
                    let unconfirmed = self.ledger.unconfirmed();
                    for hash in batch {
                        self.stats
                            .inc(StatType::BoundedBacklog, DetailType::Scanned);
                        // Erase if the block is either confirmed or missing
                        if !unconfirmed.block_exists(&hash) {
                            self.to_erase.push(hash);
                            self.state.lock().index.erase_hash(&hash);
                        }
                        self.last = hash;
                    }
                }

                if !self.to_erase.is_empty() {
                    {
                        let mut state = self.state.lock();
                        for hash in self.to_erase.drain(..) {
                            state.index.erase_hash(&hash);
                        }
                    }
                    self.state.notify_all();
                }
            }
            state = self.state.lock();
        }
    }
}
