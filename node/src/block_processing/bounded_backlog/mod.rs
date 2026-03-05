mod confirmed_scan;
mod ledger_adapter;
mod rate_limit_thread;
mod rollback_loop;
mod stats;

use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

use rsnano_ledger::{AnySet, Ledger, OwningAnySet, ProcessResult};
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::{Account, AccountInfo, BlockHash, ConfirmationHeightInfo, SavedBlock};
use rsnano_utils::{
    CancellationToken,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use super::{backlog_index::BacklogEntry, backlog_scan::UnconfirmedInfo};
use crate::{
    block_processing::bounded_backlog::{
        confirmed_scan::RecentlyConfirmedScan,
        rate_limit_thread::RateLimitThreadFactory,
        rollback_loop::{BoundedBacklogState, RollbackLoop},
        stats::BoundedBacklogStats,
    },
    consensus::election_schedulers::priority::prio_bucket_index,
};
pub(crate) use ledger_adapter::BoundedBacklogLedgerAdapter;

#[derive(Clone, Debug, PartialEq)]
pub struct BoundedBacklogConfig {
    pub max_backlog: u64,
    /// The rollback is done in batches of this configured size
    pub rollback_batch_size: usize,
    pub scan_rate: usize,
}

impl Default for BoundedBacklogConfig {
    fn default() -> Self {
        Self {
            max_backlog: 100_000,
            rollback_batch_size: 32,
            scan_rate: 64,
        }
    }
}

pub struct BoundedBacklog {
    process_thread: Mutex<Option<JoinHandle<()>>>,
    scan_thread: Mutex<Option<rsnano_utils::thread_factory::JoinHandle>>,
    cancel_token: CancellationToken,
    rate_limit_thread_factory: RateLimitThreadFactory,
    stats: Arc<BoundedBacklogStats>,
    state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    ledger: Arc<Ledger>,
    config: BoundedBacklogConfig,
    can_roll_back: Mutex<Option<Box<dyn Fn(&BlockHash) -> bool + Send + Sync>>>,
}

impl BoundedBacklog {
    pub(crate) fn new(config: BoundedBacklogConfig, ledger: Arc<Ledger>) -> Self {
        let state = Arc::new(NullableCondvarMutex::new(BoundedBacklogState::new(
            config.clone(),
        )));

        let stats = Arc::new(BoundedBacklogStats::default());

        Self {
            process_thread: Mutex::new(None),
            scan_thread: Mutex::new(None),
            cancel_token: CancellationToken::new(),
            rate_limit_thread_factory: Default::default(),
            stats,
            state,
            ledger,
            config,
            can_roll_back: Mutex::new(None),
        }
    }

    pub fn new_null() -> Self {
        let config = BoundedBacklogConfig::default();
        let ledger = Arc::new(Ledger::new_null());

        Self::new(config, ledger)
    }

    pub fn start(&self) {
        debug_assert!(self.process_thread.lock().unwrap().is_none());

        let can_roll_back = self
            .can_roll_back
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Box::new(|_| true));

        let rollback_loop = RollbackLoop {
            state: self.state.clone(),
            stats: self.stats.clone(),
            ledger: self.ledger.clone(),
            can_roll_back,
        };

        let handle = std::thread::Builder::new()
            .name("Bounded backlog".to_owned())
            .spawn(move || rollback_loop.run_process())
            .unwrap();

        *self.process_thread.lock().unwrap() = Some(handle);

        let mut confirmed_scan = RecentlyConfirmedScan::new(
            self.state.clone(),
            self.stats.clone(),
            self.ledger.clone(),
            self.config.rollback_batch_size,
        );

        let handle = self.rate_limit_thread_factory.spawn(
            "Bounded b scan",
            self.cancel_token.clone(),
            self.config.scan_rate,
            self.config.rollback_batch_size,
            move || {
                confirmed_scan.scan_batch();
            },
        );

        *self.scan_thread.lock().unwrap() = Some(handle);
    }

    pub fn stop(&self) {
        self.cancel_token.cancel();

        self.state.lock().stopped = true;
        self.state.notify_all();

        let handle = self.process_thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }

        let handle = self.scan_thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
    }

    // Give other components a chance to veto a rollback
    pub fn can_roll_back(&self, f: impl Fn(&BlockHash) -> bool + Send + Sync + 'static) {
        *self.can_roll_back.lock().unwrap() = Some(Box::new(f));
    }

    pub fn set_cooldown(&self, cool_down: bool) {
        self.state.lock().cool_down = cool_down;
        self.state.notify_all();
    }

    pub fn activate_batch(&self, batch: &[UnconfirmedInfo]) {
        let mut any = self.ledger.any();
        for info in batch {
            self.activate(&mut any, &info.account, &info.account_info, &info.conf_info);
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

    pub fn erase_accounts(&self, accounts: &[Account]) {
        let mut guard = self.state.lock();
        for account in accounts {
            guard.index.erase_account(account);
        }
    }

    pub fn erase_hashes(&self, accounts: impl IntoIterator<Item = BlockHash>) {
        let mut guard = self.state.lock();
        for account in accounts.into_iter() {
            guard.index.erase_hash(&account);
        }
    }

    fn contains(&self, hash: &BlockHash) -> bool {
        self.state.lock().index.contains(hash)
    }

    fn activate<'a>(
        &'a self,
        any: &mut OwningAnySet<'a>,
        _account: &Account,
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

        self.state.lock().index.insert(BacklogEntry {
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
        debug_assert!(self.process_thread.lock().unwrap().is_none());
        debug_assert!(self.scan_thread.lock().unwrap().is_none());
    }
}

impl ContainerInfoProvider for BoundedBacklog {
    fn container_info(&self) -> ContainerInfo {
        let guard = self.state.lock();
        ContainerInfo::builder()
            .leaf("backlog", guard.index.len(), 0)
            .node("index", guard.index.container_info())
            .finish()
    }
}

impl StatsSource for BoundedBacklog {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn collects_stats() {
        let bounded_backlog = BoundedBacklog::new_null();
        bounded_backlog
            .stats
            .loop_scan
            .fetch_add(1, Ordering::Relaxed);

        let mut result = StatsCollection::new();
        bounded_backlog.collect_stats(&mut result);

        assert_eq!(result.get("bounded_backlog", "loop_scan"), 1);
    }
}
