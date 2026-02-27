use std::{
    cmp::min,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use tracing::warn;

use rsnano_ledger::Ledger;
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;
use rsnano_utils::{
    stats::{DetailType, StatType, Stats},
    sync::backpressure_channel::Sender,
};

use super::{BoundedBacklogConfig, BoundedBacklogState};
use crate::block_processing::LedgerEvent;

pub(super) struct RollbackLoop {
    pub(super) state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    pub(super) config: BoundedBacklogConfig,
    pub(super) stats: Arc<Stats>,
    pub(super) ledger: Arc<Ledger>,
    pub(super) can_roll_back: RwLock<Box<dyn Fn(&BlockHash) -> bool + Send + Sync>>,
    pub(super) publish_event: Mutex<Option<Sender<LedgerEvent>>>,
}

impl RollbackLoop {
    pub(super) fn run_process(&self) {
        let mut state = self.state.lock();
        while !state.stopped {
            state = self
                .state
                .wait_timeout_while(state, Duration::from_secs(1), |i| {
                    !i.stopped && !i.predicate(self.ledger.backlog_size())
                })
                .0;

            if state.stopped {
                return;
            }

            self.stats.inc(StatType::BoundedBacklog, DetailType::Loop);

            // Calculate the number of targets to rollback
            let backlog = self.ledger.backlog_size();
            let target_count = backlog.saturating_sub(self.config.max_backlog);
            let can_roll_back = self.can_roll_back.read().unwrap();

            let targets = state.gather_targets(
                min(target_count as usize, self.config.batch_size),
                &*can_roll_back,
            );

            if !targets.is_empty() {
                drop(state);
                self.stats.add(
                    StatType::BoundedBacklog,
                    DetailType::GatheredTargets,
                    targets.len() as u64,
                );

                let processed = self.roll_back(&targets, target_count as usize, &*can_roll_back);
                state = self.state.lock();

                // Erase rolled back blocks from the index
                for hash in &processed {
                    state.index.erase_hash(hash);
                }
            } else {
                // Cooldown, this should not happen in normal operation
                self.stats
                    .inc(StatType::BoundedBacklog, DetailType::NoTargets);
                state = self
                    .state
                    .wait_timeout_while(state, Duration::from_millis(100), |i| !i.stopped)
                    .0;
            }
        }
    }

    fn roll_back(
        &self,
        targets: &[BlockHash],
        max_rollbacks: usize,
        can_roll_back: impl Fn(&BlockHash) -> bool,
    ) -> Vec<BlockHash> {
        let results = self
            .ledger
            .roll_back_batch(targets, max_rollbacks, can_roll_back);

        let mut processed_hashes = Vec::new();
        for result in results.iter() {
            if !result.rolled_back.is_empty() {
                for h in &result.rolled_back {
                    processed_hashes.push(h.hash());
                }
            } else {
                processed_hashes.push(result.target_hash);
            }
        }

        if let Err(e) = self
            .publish_event
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(LedgerEvent::BlocksRolledBack(results))
        {
            warn!("Failed to publish rolled back event: {e:?}")
        }

        processed_hashes
    }
}
