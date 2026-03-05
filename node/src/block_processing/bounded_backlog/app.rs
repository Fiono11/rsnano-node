use std::{
    sync::{Arc, MutexGuard},
    time::Duration,
};

use rsnano_ledger::{AnySet, Ledger, OwningAnySet, ProcessResult};
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::{Account, AccountInfo, BlockHash, ConfirmationHeightInfo, SavedBlock};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use super::logic::BoundedBacklogLogic;
use crate::{
    block_processing::{backlog_index::BacklogEntry, backlog_scan::UnconfirmedInfo},
    consensus::election_schedulers::priority::prio_bucket_index,
};

/// Continuously rolls back unconfirmed blocks with the lowest priority
/// if the backlog exceeds the configured limit
pub(crate) struct BoundedBacklogApp {
    pub(super) logic: Arc<NullableCondvarMutex<BoundedBacklogLogic>>,
    pub(super) ledger: Arc<Ledger>,
}

impl BoundedBacklogApp {
    pub fn set_cooldown(&self, cool_down: bool) {
        self.logic.lock().set_cool_down(cool_down);
        self.logic.notify_all();
    }

    pub fn stop(&self) {
        self.logic.lock().stop();
        self.logic.notify_all();
    }

    pub(crate) fn run(&self) {
        let mut logic = self.logic.lock();
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

    pub fn activate_batch(&self, batch: &[UnconfirmedInfo]) {
        let mut any = self.ledger.any();
        for info in batch {
            self.activate(&mut any, &info.account_info, &info.conf_info);
        }
    }

    fn activate<'a>(
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
            if self.contains(&blk.hash()) {
                break;
            }

            let inserted = self.insert(any, &blk);

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

    /// Track unconfirmed blocks
    pub fn insert_processed(&self, batch: &[ProcessResult]) {
        let any = self.ledger.any();
        for result in batch {
            if result.status.is_ok()
                && let Some(block) = &result.saved_block
            {
                self.insert(&any, block);
            }
        }
    }

    fn insert(&self, any: &impl AnySet, block: &SavedBlock) -> bool {
        let priority = any.block_priority(block);
        let bucket_index = prio_bucket_index(priority.balance);

        self.logic.lock().index.insert(BacklogEntry {
            hash: block.hash(),
            account: block.account(),
            bucket_index,
            priority: priority.time,
        })
    }

    fn contains(&self, hash: &BlockHash) -> bool {
        self.logic.lock().index.contains(hash)
    }

    pub fn remove(&self, confirmed: &Vec<(SavedBlock, BlockHash)>) {
        // Remove confirmed blocks from the backlog
        self.erase_hashes(confirmed.iter().map(|i| i.0.hash()));
    }

    pub fn erase_hashes(&self, accounts: impl IntoIterator<Item = BlockHash>) {
        let mut guard = self.logic.lock();
        for account in accounts.into_iter() {
            guard.index.erase_hash(&account);
        }
    }

    pub fn erase_accounts(&self, accounts: &[Account]) {
        let mut guard = self.logic.lock();
        for account in accounts {
            guard.index.erase_account(account);
        }
    }
}

impl StatsSource for BoundedBacklogApp {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.logic.lock().collect_stats(result);
    }
}

impl ContainerInfoProvider for BoundedBacklogApp {
    fn container_info(&self) -> ContainerInfo {
        let guard = self.logic.lock();
        ContainerInfo::builder()
            .leaf("backlog", guard.index.len(), 0)
            .node("index", guard.index.container_info())
            .finish()
    }
}
