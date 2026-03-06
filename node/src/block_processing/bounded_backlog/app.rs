use std::{
    sync::{Arc, MutexGuard},
    time::Duration,
};

use rsnano_ledger::{AnySet, Ledger, LedgerEvent, OwningAnySet};
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::{AccountInfo, BlockHash, ConfirmationHeightInfo};
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use super::logic::BoundedBacklogLogic;
use crate::block_processing::{
    LedgerPipelineEvent, backlog_scan::UnconfirmedInfo, bounded_backlog::BoundedBacklogConfig,
};
use tracing::info;

/// Continuously rolls back unconfirmed blocks with the lowest priority
/// if the backlog exceeds the configured limit
/// This struct belongs to the application layer
pub struct BoundedBacklog {
    logic: NullableCondvarMutex<BoundedBacklogLogic>,
    ledger: Arc<Ledger>,
}

impl BoundedBacklog {
    pub fn new(config: BoundedBacklogConfig, ledger: Arc<Ledger>) -> Self {
        let logic = NullableCondvarMutex::new(BoundedBacklogLogic::new(config));
        Self { logic, ledger }
    }

    pub fn new_null() -> Self {
        Self::new(Default::default(), Ledger::new_null().into())
    }

    pub fn set_cooldown(&self, cool_down: bool) {
        self.logic.lock().set_cool_down(cool_down);
        self.logic.notify_all();
    }

    pub fn stop(&self) {
        self.logic.lock().stop();
        self.logic.notify_all();
    }

    pub(crate) fn run_loop(&self) {
        let mut logic = self.logic.lock();
        info!(
            "Bounded backlog enabled: max backlog={}, batch_size={}",
            logic.config.max_backlog, logic.config.rollback_batch_size,
        );

        let mut targets = Vec::with_capacity(logic.rollback_batch_size());

        while !logic.stopped() {
            logic.set_current_backlog_size(self.ledger.backlog_size());
            logic = self
                .logic
                .wait_timeout_while(logic, Duration::from_secs(1), |i| {
                    !i.stopped() && !i.rollback_needed()
                })
                .0;

            logic = self.run_one(logic, &mut targets);
        }
    }

    fn run_one<'a>(
        &'a self,
        mut state: MutexGuard<'a, BoundedBacklogLogic>,
        targets: &mut Vec<BlockHash>,
    ) -> MutexGuard<'a, BoundedBacklogLogic> {
        if state.stopped() || !state.rollback_needed() {
            return state;
        }

        state.gather_targets(targets);

        if !targets.is_empty() {
            let target_count = state.rollback_target_count();
            drop(state);

            self.ledger
                .roll_back_batch(&*targets, target_count as usize);

            state = self.logic.lock();
        }

        state
    }

    fn unconfirmed_accounts_found(&self, batch: &[UnconfirmedInfo]) {
        let mut any = self.ledger.any();
        for info in batch {
            self.process_unconfirmed_account(&mut any, &info.account_info, &info.conf_info);
        }
    }

    fn process_unconfirmed_account<'a>(
        &'a self,
        any: &mut OwningAnySet<'a>,
        account_info: &AccountInfo,
        conf_info: &ConfirmationHeightInfo,
    ) {
        debug_assert!(conf_info.frontier != account_info.head);

        // Insert blocks into the index starting from the account head block
        let mut block = any.get_block(&account_info.head);

        while let Some(blk) = block {
            // We reached the confirmed frontier, no need to track more blocks
            if blk.hash() == conf_info.frontier {
                break;
            }

            // Check if the block is already in the backlog, avoids unnecessary ledger lookups
            if self.logic.lock().index.contains(&blk.hash()) {
                break;
            }

            let priority = any.block_priority(&blk);
            let inserted = self.logic.lock().insert(&blk, priority);

            // If the block was not inserted, we already have it in the backlog
            if !inserted {
                break;
            }

            if any.should_refresh() {
                *any = self.ledger.any();
            }

            block = any.get_block(&blk.previous());
        }
    }
}

impl StatsSource for BoundedBacklog {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.logic.lock().collect_stats(result);
    }
}

impl ContainerInfoProvider for BoundedBacklog {
    fn container_info(&self) -> ContainerInfo {
        self.logic.lock().container_info()
    }
}

impl EventHandler<LedgerPipelineEvent> for BoundedBacklog {
    fn handle(&self, event: &LedgerPipelineEvent) {
        match event {
            LedgerPipelineEvent::Ledger(event) => match event {
                LedgerEvent::BlocksProcessed(results) => {
                    self.logic.lock().insert_processed(results);
                }
                LedgerEvent::BlocksConfirmed(confirmed) => {
                    self.logic
                        .lock()
                        .remove_batch(confirmed.iter().map(|i| i.0.hash()));
                }
                LedgerEvent::BlocksRolledBack(rolled_back) => {
                    self.logic.lock().remove_batch(rolled_back.hashes());
                }
            },
            LedgerPipelineEvent::UnconfirmedFound(unconfirmed) => {
                self.unconfirmed_accounts_found(unconfirmed);
            }
            _ => (),
        }
    }
}
