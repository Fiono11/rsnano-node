mod app;
mod ledger_adapter;
mod logic;

use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

use rsnano_ledger::{AnySet, Ledger, OwningAnySet, ProcessResult};
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::{Account, AccountInfo, BlockHash, ConfirmationHeightInfo, SavedBlock};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use super::{backlog_index::BacklogEntry, backlog_scan::UnconfirmedInfo};
use crate::{
    block_processing::bounded_backlog::{app::BoundedBacklogApp, logic::BoundedBacklogLogic},
    consensus::election_schedulers::priority::prio_bucket_index,
};
pub(crate) use ledger_adapter::BoundedBacklogLedgerAdapter;
pub use logic::BoundedBacklogConfig;

pub struct BoundedBacklog {
    thread: Mutex<Option<JoinHandle<()>>>,
    logic: Arc<NullableCondvarMutex<BoundedBacklogLogic>>,
    ledger: Arc<Ledger>,
}

impl BoundedBacklog {
    pub(crate) fn new(config: BoundedBacklogConfig, ledger: Arc<Ledger>) -> Self {
        let logic = Arc::new(NullableCondvarMutex::new(BoundedBacklogLogic::new(
            config.clone(),
        )));

        Self {
            thread: Mutex::new(None),
            logic,
            ledger,
        }
    }

    pub fn new_null() -> Self {
        let config = BoundedBacklogConfig::default();
        let ledger = Arc::new(Ledger::new_null());

        Self::new(config, ledger)
    }

    pub fn start(&self) {
        debug_assert!(self.thread.lock().unwrap().is_none());

        let app = BoundedBacklogApp {
            logic: self.logic.clone(),
            ledger: self.ledger.clone(),
        };

        let handle = std::thread::Builder::new()
            .name("Bounded backlog".to_owned())
            .spawn(move || app.run())
            .unwrap();

        *self.thread.lock().unwrap() = Some(handle);
    }

    pub fn stop(&self) {
        self.logic.lock().stop();
        self.logic.notify_all();

        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
    }

    pub fn set_cooldown(&self, cool_down: bool) {
        self.logic.lock().set_cool_down(cool_down);
        self.logic.notify_all();
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

    pub fn erase_accounts(&self, accounts: &[Account]) {
        let mut guard = self.logic.lock();
        for account in accounts {
            guard.index.erase_account(account);
        }
    }

    pub fn erase_hashes(&self, accounts: impl IntoIterator<Item = BlockHash>) {
        let mut guard = self.logic.lock();
        for account in accounts.into_iter() {
            guard.index.erase_hash(&account);
        }
    }

    fn contains(&self, hash: &BlockHash) -> bool {
        self.logic.lock().index.contains(hash)
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

    pub fn insert(&self, any: &impl AnySet, block: &SavedBlock) -> bool {
        let priority = any.block_priority(block);
        let bucket_index = prio_bucket_index(priority.balance);

        self.logic.lock().index.insert(BacklogEntry {
            hash: block.hash(),
            account: block.account(),
            bucket_index,
            priority: priority.time,
        })
    }

    pub fn remove(&self, confirmed: &Vec<(SavedBlock, BlockHash)>) {
        // Remove confirmed blocks from the backlog
        self.erase_hashes(confirmed.iter().map(|i| i.0.hash()));
    }
}

impl Drop for BoundedBacklog {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        debug_assert!(self.thread.lock().unwrap().is_none());
    }
}

impl StatsSource for BoundedBacklog {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.logic.lock().collect_stats(result);
    }
}

impl ContainerInfoProvider for BoundedBacklog {
    fn container_info(&self) -> ContainerInfo {
        let guard = self.logic.lock();
        ContainerInfo::builder()
            .leaf("backlog", guard.index.len(), 0)
            .node("index", guard.index.container_info())
            .finish()
    }
}
