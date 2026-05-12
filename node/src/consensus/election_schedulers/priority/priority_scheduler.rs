use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
};

use rsnano_ledger::{AnySet, ConfirmedSet, Ledger};
use rsnano_nullable_clock::SteadyClock;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{Account, AccountInfo, BlockHash, ConfirmationHeightInfo};
use rsnano_utils::{
    container_info::ContainerInfo,
    stats::{DetailType, StatType, Stats, StatsCollection, StatsSource},
};

use super::{PriorityBucketConfig, prio_bucket_count};
use crate::{
    block_processing::backlog_scan::UnconfirmedInfo,
    consensus::{
        AecService,
        election_schedulers::priority::{
            BucketInsertError, Eviction, priority_buckets::PriorityBuckets,
        },
    },
};

pub struct PriorityScheduler {
    stopped: Mutex<bool>,
    condition: Condvar,
    stats: Arc<Stats>,
    buckets: Mutex<PriorityBuckets>,
    thread: Mutex<Option<JoinHandle<()>>>,
    clock: Arc<SteadyClock>,
    aec: Arc<AecService>,
    ledger: Arc<Ledger>,
    activate_listener: OutputListenerMt<Account>,
}

impl PriorityScheduler {
    pub(crate) fn new(
        config: PriorityBucketConfig,
        stats: Arc<Stats>,
        active_elections: Arc<AecService>,
        ledger: Arc<Ledger>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        let buckets = PriorityBuckets::new(prio_bucket_count(), config);

        Self {
            thread: Mutex::new(None),
            stopped: Mutex::new(false),
            condition: Condvar::new(),
            buckets: Mutex::new(buckets),
            stats,
            ledger,
            clock,
            aec: active_elections,
            activate_listener: Default::default(),
        }
    }

    pub fn track_activate(&self) -> Arc<OutputTrackerMt<Account>> {
        self.activate_listener.track()
    }

    pub fn stop(&self) {
        *self.stopped.lock().unwrap() = true;
        self.condition.notify_all();
        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
    }

    pub fn notify(&self) {
        self.condition.notify_all();
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.buckets.lock().unwrap().contains(hash)
    }

    pub fn activate(&self, any: &impl AnySet, account: &Account) {
        if self.activate_listener.is_tracked() {
            self.activate_listener.emit(*account);
        }
        debug_assert!(!account.is_zero());
        if let Some(account_info) = any.get_account(account) {
            let conf_info = any.confirmed().get_conf_info(account).unwrap_or_default();

            if conf_info.height < account_info.block_count {
                self.activate_with_info(any, &account_info, &conf_info);
                return;
            }
        };

        self.stats
            .inc(StatType::ElectionScheduler, DetailType::ActivateSkip);
    }

    pub fn activate_batch(&self, unconfirmed: &[UnconfirmedInfo]) {
        let any = self.ledger.any();
        for info in unconfirmed {
            self.activate_with_info(&any, &info.account_info, &info.conf_info);
        }
    }

    pub fn activate_with_info(
        &self,
        any: &impl AnySet,
        account_info: &AccountInfo,
        conf_info: &ConfirmationHeightInfo,
    ) {
        debug_assert!(conf_info.frontier != account_info.head);

        let next_unconfirmed_hash = match conf_info.height {
            0 => account_info.open_block,
            _ => {
                match any.block_successor(&conf_info.frontier) {
                    Some(h) => h,
                    None => {
                        // This can happen if the bounded backlog did a rollback
                        return;
                    }
                }
            }
        };

        let Some(block) = any.get_block(&next_unconfirmed_hash) else {
            return;
        };

        if !any.dependencies_confirmed(&block) {
            self.stats
                .inc(StatType::ElectionScheduler, DetailType::ActivateFailed);
            return;
        }

        #[cfg(feature = "ledger_snapshots")]
        if any.is_forked(&block.qualified_root()) {
            self.stats
                .inc(StatType::ElectionScheduler, DetailType::ActivateFailed);
            return;
        }

        let priority = any.block_priority(&block);

        let insert_result = self.buckets.lock().unwrap().insert(priority, block);

        match insert_result {
            Ok(Eviction::None) => {}
            Ok(Eviction::Evicted) => {
                self.stats
                    .inc(StatType::ElectionScheduler, DetailType::Evicted);
            }
            Err(BucketInsertError::Duplicate) => {
                self.stats
                    .inc(StatType::ElectionScheduler, DetailType::Duplicate);
            }
            Err(BucketInsertError::PriorityTooLow) => {
                self.stats
                    .inc(StatType::ElectionScheduler, DetailType::ActivateFull);
            }
        }

        if insert_result.is_ok() {
            self.stats
                .inc(StatType::ElectionScheduler, DetailType::Activated);
            self.condition.notify_all();
        }
    }

    pub fn len(&self) -> usize {
        self.buckets.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn run(&self) {
        let mut stopped = self.stopped.lock().unwrap();
        while !*stopped {
            stopped = self
                .condition
                .wait_while(stopped, |s| !*s && !self.predicate())
                .unwrap();

            if !*stopped {
                drop(stopped);
                self.run_one();
                stopped = self.stopped.lock().unwrap();
            }
        }
    }

    fn predicate(&self) -> bool {
        let buckets = self.buckets.lock().unwrap();
        self.aec.check_vacancy(&*buckets)
    }

    fn run_one(&self) {
        self.stats
            .inc(StatType::ElectionScheduler, DetailType::Loop);

        let now = self.clock.now();
        let mut buckets = self.buckets.lock().unwrap();
        self.aec.refill(&mut *buckets, now);
    }

    pub fn container_info(&self) -> ContainerInfo {
        let mut bucket_infos = ContainerInfo::builder();

        for (id, bucket) in self.buckets.lock().unwrap().iter().enumerate() {
            bucket_infos = bucket_infos.leaf(id.to_string(), bucket.len(), 0);
        }

        ContainerInfo::builder()
            .node("blocks", bucket_infos.finish())
            .finish()
    }
}

impl Drop for PriorityScheduler {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        debug_assert!(self.thread.lock().unwrap().is_none());
    }
}

pub trait PrioritySchedulerExt {
    fn start(&self);
}

impl PrioritySchedulerExt for Arc<PriorityScheduler> {
    fn start(&self) {
        debug_assert!(self.thread.lock().unwrap().is_none());

        let self_l = Arc::clone(self);
        *self.thread.lock().unwrap() = Some(
            std::thread::Builder::new()
                .name("Sched Priority".to_string())
                .spawn(Box::new(move || {
                    self_l.run();
                }))
                .unwrap(),
        );
    }
}

impl StatsSource for PriorityScheduler {
    fn collect_stats(&self, result: &mut StatsCollection) {
        let guard = self.buckets.lock().unwrap();
        guard.bucket_stats.collect_stats(result);
        guard.collect_stats(result);
    }
}
