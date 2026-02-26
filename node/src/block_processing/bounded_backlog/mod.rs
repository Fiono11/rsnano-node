mod ledger_adapter;
mod scan_loop;

use std::{
    cmp::min,
    sync::{Arc, Mutex, RwLock},
    thread::JoinHandle,
    time::Duration,
};

use tracing::warn;

use rsnano_ledger::{AnySet, Ledger, OwningAnySet};
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::{Account, AccountInfo, BlockHash, ConfirmationHeightInfo, SavedBlock};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, StatType, Stats, StatsCollection, StatsSource},
    sync::backpressure_channel::{Sender, channel},
};

use super::{
    LedgerEvent, ProcessedResult,
    backlog_index::{BacklogEntry, BacklogIndex},
    backlog_scan::UnconfirmedInfo,
};
use crate::{
    block_processing::bounded_backlog::scan_loop::ScanLoop,
    consensus::election_schedulers::priority::{prio_bucket_count, prio_bucket_index},
};
pub(crate) use ledger_adapter::BoundedBacklogLedgerAdapter;

#[derive(Clone, Debug, PartialEq)]
pub struct BoundedBacklogConfig {
    pub max_backlog: u64,
    pub batch_size: usize,
    pub scan_rate: usize,
}

impl Default for BoundedBacklogConfig {
    fn default() -> Self {
        Self {
            max_backlog: 100_000,
            batch_size: 32,
            scan_rate: 64,
        }
    }
}

pub struct BoundedBacklog {
    process_thread: Mutex<Option<JoinHandle<()>>>,
    scan_thread: Mutex<Option<JoinHandle<()>>>,
    backlog_impl: Arc<BoundedBacklogImpl>,
}

impl BoundedBacklog {
    pub(crate) fn new(
        config: BoundedBacklogConfig,
        ledger: Arc<Ledger>,
        stats: Arc<Stats>,
        clock: Arc<SteadyClock>,
        publish_event: Sender<LedgerEvent>,
    ) -> Self {
        let backlog_impl = Arc::new(BoundedBacklogImpl {
            state: NullableCondvarMutex::new(BoundedBacklogState::new(config.clone())).into(),
            config,
            stats,
            ledger,
            clock,
            can_roll_back: RwLock::new(Box::new(|_| true)),
            publish_event: Mutex::new(Some(publish_event)),
        });

        Self {
            backlog_impl,
            process_thread: Mutex::new(None),
            scan_thread: Mutex::new(None),
        }
    }

    pub fn new_null() -> Self {
        let config = BoundedBacklogConfig::default();
        let ledger = Arc::new(Ledger::new_null());
        let stats = Arc::new(Stats::default());
        let clock = Arc::new(SteadyClock::new_null());
        let (sender, _) = channel(0);

        Self::new(config, ledger, stats, clock, sender)
    }

    pub fn start(&self) {
        debug_assert!(self.process_thread.lock().unwrap().is_none());

        let backlog_impl = self.backlog_impl.clone();
        let handle = std::thread::Builder::new()
            .name("Bounded backlog".to_owned())
            .spawn(move || backlog_impl.run_process())
            .unwrap();
        *self.process_thread.lock().unwrap() = Some(handle);

        let scan_loop = ScanLoop::new(
            self.backlog_impl.state.clone(),
            self.backlog_impl.stats.clone(),
            self.backlog_impl.ledger.clone(),
            self.backlog_impl.clock.clone(),
            self.backlog_impl.config.scan_rate,
            self.backlog_impl.config.batch_size,
        );
        let handle = std::thread::Builder::new()
            .name("Bounded b scan".to_owned())
            .spawn(move || scan_loop.run())
            .unwrap();
        *self.scan_thread.lock().unwrap() = Some(handle);
    }

    pub fn stop(&self) {
        self.backlog_impl.state.lock().stopped = true;
        self.backlog_impl.state.notify_all();

        let handle = self.process_thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }

        let handle = self.scan_thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
        drop(self.backlog_impl.publish_event.lock().unwrap().take());
    }

    // Give other components a chance to veto a rollback
    pub fn can_roll_back(&self, f: impl Fn(&BlockHash) -> bool + Send + Sync + 'static) {
        *self.backlog_impl.can_roll_back.write().unwrap() = Box::new(f);
    }

    pub fn set_cooldown(&self, cool_down: bool) {
        self.backlog_impl.state.lock().cool_down = cool_down;
        self.backlog_impl.state.notify_all();
    }

    pub fn activate_batch(&self, batch: &[UnconfirmedInfo]) {
        let mut any = self.backlog_impl.ledger.any();
        for info in batch {
            self.activate(&mut any, &info.account, &info.account_info, &info.conf_info);
        }
    }

    /// Track unconfirmed blocks
    pub fn insert_processed(&self, batch: &[ProcessedResult]) {
        let any = self.backlog_impl.ledger.any();
        for result in batch {
            if result.status.is_ok()
                && let Some(block) = &result.saved_block
            {
                self.insert(&any, block);
            }
        }
    }

    pub fn erase_accounts(&self, accounts: &[Account]) {
        let mut guard = self.backlog_impl.state.lock();
        for account in accounts {
            guard.index.erase_account(account);
        }
    }

    pub fn erase_hashes(&self, accounts: impl IntoIterator<Item = BlockHash>) {
        let mut guard = self.backlog_impl.state.lock();
        for account in accounts.into_iter() {
            guard.index.erase_hash(&account);
        }
    }

    fn contains(&self, hash: &BlockHash) -> bool {
        let guard = self.backlog_impl.state.lock();
        guard.index.contains(hash)
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
                *any = self.backlog_impl.ledger.any();
            }

            block = any.get_block(&blk.previous());
        }
    }

    pub fn insert(&self, any: &impl AnySet, block: &SavedBlock) -> bool {
        let priority = any.block_priority(block);
        let bucket_index = prio_bucket_index(priority.balance);

        self.backlog_impl.state.lock().index.insert(BacklogEntry {
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
        let guard = self.backlog_impl.state.lock();
        ContainerInfo::builder()
            .leaf("backlog", guard.index.len(), 0)
            .node("index", guard.index.container_info())
            .finish()
    }
}

impl StatsSource for BoundedBacklog {
    fn collect_stats(&self, _result: &mut StatsCollection) {}
}

struct BoundedBacklogImpl {
    state: Arc<NullableCondvarMutex<BoundedBacklogState>>,
    config: BoundedBacklogConfig,
    stats: Arc<Stats>,
    ledger: Arc<Ledger>,
    can_roll_back: RwLock<Box<dyn Fn(&BlockHash) -> bool + Send + Sync>>,
    clock: Arc<SteadyClock>,
    publish_event: Mutex<Option<Sender<LedgerEvent>>>,
}

impl BoundedBacklogImpl {
    fn run_process(&self) {
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

struct BoundedBacklogState {
    stopped: bool,
    cool_down: bool,
    index: BacklogIndex,
    config: BoundedBacklogConfig,
    bucket_count: usize,
}

impl BoundedBacklogState {
    fn new(config: BoundedBacklogConfig) -> Self {
        Self {
            stopped: false,
            cool_down: false,
            index: BacklogIndex::new(prio_bucket_count()),
            config,
            bucket_count: prio_bucket_count(),
        }
    }

    fn predicate(&self, backlog_size: u64) -> bool {
        if self.cool_down {
            return false;
        }

        // Both ledger and tracked backlog must be over the threshold
        let max_backlog = self.config.max_backlog;
        debug_assert!(
            max_backlog > 0,
            "Should be fully disabled if max_backlog is 0"
        );

        backlog_size > max_backlog && self.index.len() > max_backlog as usize
    }

    fn gather_targets(
        &self,
        max_count: usize,
        can_rollback: impl Fn(&BlockHash) -> bool,
    ) -> Vec<BlockHash> {
        let mut targets = Vec::new();

        // Start rolling back from lowest index buckets first
        for bucket in 0..self.bucket_count {
            // Only start rolling back if the bucket is over the threshold of unconfirmed blocks
            if self.index.len_of_bucket(bucket) > self.bucket_threshold() {
                let count = min(max_count, self.config.batch_size);
                let top = self.index.top(bucket, count, |hash| {
                    // Only rollback if the block is not being used by the node
                    can_rollback(hash)
                });
                targets.extend(top);
            }
        }
        targets
    }

    fn bucket_threshold(&self) -> usize {
        self.config.max_backlog as usize / self.bucket_count
    }
}
