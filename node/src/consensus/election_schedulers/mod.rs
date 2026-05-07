mod hinted_scheduler;
mod manual_scheduler;
mod optimistic;
pub mod priority;

pub use hinted_scheduler::*;
pub use manual_scheduler::*;
pub use optimistic::*;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

use rsnano_ledger::{Ledger, LedgerEvent};
use rsnano_nullable_clock::SteadyClock;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{Account, BlockHash, SavedBlock};
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{Stats, StatsCollection, StatsSource},
};

use super::{AecService, VoteCache};
use crate::{
    block_processing::{LedgerPipelineEvent, backlog_scan::UnconfirmedInfo},
    cementation::ConfirmingSet,
    config::NodeConfig,
    representatives::OnlineReps,
};
use priority::{PriorityScheduler, PrioritySchedulerExt};

pub struct ElectionSchedulers {
    pub priority: Arc<PriorityScheduler>,
    pub optimistic: Arc<OptimisticScheduler>,
    pub hinted: Arc<HintedScheduler>,
    pub manual: Arc<ManualScheduler>,
    notify_listener: OutputListenerMt<()>,
    config: NodeConfig,
    ledger: Arc<Ledger>,
    optimistic_thread: Mutex<Option<JoinHandle<()>>>,
    tx_activate: Mutex<Option<mpsc::SyncSender<Account>>>,
    ev_proc_thread: Mutex<Option<JoinHandle<()>>>,
    enqueued: AtomicU64,
    overfill: AtomicU64,
    dequeued: Arc<AtomicU64>,
}

impl ElectionSchedulers {
    pub fn new(
        config: NodeConfig,
        active_elections: Arc<AecService>,
        ledger: Arc<Ledger>,
        stats: Arc<Stats>,
        vote_cache: Arc<Mutex<VoteCache>>,
        confirming_set: Arc<ConfirmingSet>,
        online_reps: Arc<Mutex<OnlineReps>>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        let hinted = Arc::new(HintedScheduler::new(
            config.hinted_scheduler.clone(),
            active_elections.clone(),
            ledger.clone(),
            stats.clone(),
            vote_cache.clone(),
            confirming_set.clone(),
            online_reps.clone(),
            clock.clone(),
        ));

        let manual = Arc::new(ManualScheduler::new(
            stats.clone(),
            active_elections.clone(),
            clock.clone(),
            ledger.clone(),
        ));

        let optimistic_params = OptimisticSchedulerParams {
            gap_threshold: config.optimistic_scheduler.gap_threshold,
            max_candidates: config.optimistic_scheduler.max_size,
            max_elections: config.active_elections.max_elections
                * config.optimistic_scheduler.optimistic_limit_percentage
                / 100,
            activation_delay: config.optimistic_scheduler.activation_delay,
        };
        let optimistic = Arc::new(OptimisticScheduler::new(
            optimistic_params,
            active_elections.clone(),
            ledger.clone(),
            confirming_set.clone(),
            clock.clone(),
        ));

        let priority = Arc::new(PriorityScheduler::new(
            config.priority_bucket.clone(),
            stats.clone(),
            active_elections.clone(),
            clock,
        ));

        Self {
            priority,
            optimistic,
            hinted,
            manual,
            notify_listener: OutputListenerMt::new(),
            config,
            ledger,
            optimistic_thread: Mutex::new(None),
            tx_activate: Mutex::new(None),
            ev_proc_thread: Mutex::new(None),
            enqueued: AtomicU64::new(0),
            dequeued: Arc::new(AtomicU64::new(0)),
            overfill: AtomicU64::new(0),
        }
    }

    pub fn new_null() -> Self {
        let config = NodeConfig::new_test_instance();
        let active_elections = Arc::new(AecService::new_null());
        let ledger = Arc::new(Ledger::new_null());
        let stats = Arc::new(Stats::default());
        let vote_cache = Arc::new(Mutex::new(VoteCache::new(
            Default::default(),
            stats.clone(),
        )));
        let confirming_set = Arc::new(ConfirmingSet::new_null());
        let online_reps = Arc::new(Mutex::new(OnlineReps::new_test_instance()));
        let clock = Arc::new(SteadyClock::new_null());

        Self::new(
            config,
            active_elections,
            ledger,
            stats,
            vote_cache,
            confirming_set,
            online_reps,
            clock,
        )
    }

    pub fn start(&self) {
        if self.config.enable_hinted_scheduler {
            self.hinted.start();
        }
        self.manual.start();
        if self.config.enable_optimistic_scheduler {
            let optimistic = self.optimistic.clone();
            let handle = std::thread::Builder::new()
                .name("Sched Opt".to_string())
                .spawn(move || optimistic.run_loop())
                .unwrap();
            *self.optimistic_thread.lock().unwrap() = Some(handle);
        }
        if self.config.enable_priority_scheduler {
            let priority = self.priority.clone();
            let ledger = self.ledger.clone();
            let (tx, rx) = mpsc::sync_channel::<Account>(1024 * 16);
            let dequeued = self.dequeued.clone();
            let ev_proc_thread = std::thread::Builder::new()
                .name("prio scheduler queue".to_string())
                .spawn(move || {
                    // TODO batch accounts for better performance?
                    while let Ok(account) = rx.recv() {
                        dequeued.fetch_and(1, Ordering::Relaxed);
                        let any = ledger.any();
                        priority.activate(&any, &account);
                    }
                })
                .unwrap();

            *self.tx_activate.lock().unwrap() = Some(tx);
            *self.ev_proc_thread.lock().unwrap() = Some(ev_proc_thread);

            self.priority.start();
        }
    }

    pub fn stop(&self) {
        self.hinted.stop();
        self.manual.stop();
        self.optimistic.stop();
        if let Some(handle) = self.optimistic_thread.lock().unwrap().take() {
            handle.join().unwrap();
        }
        self.priority.stop();
        let tx = self.tx_activate.lock().unwrap().take();
        drop(tx);
        let handle = self.ev_proc_thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().expect("Ev proc thread should end normally");
        }
    }

    /// Does the block exist in any of the schedulers
    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.manual.contains(hash) || self.priority.contains(hash)
    }

    pub fn track_notify(&self) -> Arc<OutputTrackerMt<()>> {
        self.notify_listener.track()
    }

    pub fn notify(&self) {
        self.notify_listener.emit(());
        self.priority.notify();
        self.hinted.notify();
        self.optimistic.notify();
    }

    pub fn add_manual(&self, block: SavedBlock) {
        self.manual.push(block);
    }

    fn enqueue_activation(&self, account: Account) {
        let tx = self.tx_activate.lock().unwrap();
        if let Some(tx) = tx.as_ref() {
            match tx.try_send(account) {
                Ok(()) => {
                    self.enqueued.fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::TrySendError::Full(_)) => {
                    self.overfill.fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    unreachable!("Queue should always be open")
                }
            }
        }
    }
}

impl ContainerInfoProvider for ElectionSchedulers {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .node("hinted", self.hinted.container_info())
            .node("manual", self.manual.container_info())
            .node("optimistic", self.optimistic.container_info())
            .node("priority", self.priority.container_info())
            .finish()
    }
}

impl StatsSource for ElectionSchedulers {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.priority.collect_stats(result);
        self.optimistic.collect_stats(result);
        result.insert(
            "prio_sched_queue",
            "enqueued",
            self.enqueued.load(Ordering::Relaxed),
        );
        result.insert(
            "prio_sched_queue",
            "overfill",
            self.overfill.load(Ordering::Relaxed),
        );
        result.insert(
            "prio_sched_queue",
            "dequeued",
            self.dequeued.load(Ordering::Relaxed),
        );
    }
}

impl EventHandler<LedgerPipelineEvent> for ElectionSchedulers {
    fn handle(&self, event: &LedgerPipelineEvent) {
        if let LedgerPipelineEvent::Ledger(event) = event {
            match event {
                LedgerEvent::BlocksProcessed(results) => {
                    // Activate accounts with fresh blocks
                    for result in results {
                        if result.status.is_ok() {
                            let account = result.saved_block.as_ref().unwrap().account();
                            self.enqueue_activation(account);
                        }
                    }
                }
                LedgerEvent::BlocksConfirmed(confirmed) => {
                    for (block, _) in confirmed {
                        self.enqueue_activation(block.account());
                        if let Some(destination) = block.destination()
                            && !destination.is_zero()
                            && destination != block.account()
                        {
                            self.enqueue_activation(destination);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl EventHandler<Vec<UnconfirmedInfo>> for ElectionSchedulers {
    fn handle(&self, unconfirmed: &Vec<UnconfirmedInfo>) {
        let any = self.ledger.any();
        for info in unconfirmed {
            self.optimistic.activate(
                &info.account,
                info.account_info.block_count,
                info.conf_info.height,
            );
            self.priority
                .activate_with_info(&any, &info.account_info, &info.conf_info);
        }
    }
}
