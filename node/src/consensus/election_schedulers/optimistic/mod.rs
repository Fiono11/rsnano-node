use std::{
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Instant,
};

use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Account, AccountInfo, ConfirmationHeightInfo};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, StatType, Stats},
};

use crate::{
    cementation::ConfirmingSet,
    consensus::{ActiveElectionsContainer, AecInsertRequest, election::ElectionBehavior},
};

mod candidate_queue;
mod config;
mod logic;

pub use config::OptimisticSchedulerParams;
use logic::OptimisticSchedulerLogic;

pub struct OptimisticScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    stopped: AtomicBool,
    condition: Condvar,
    logic: Mutex<OptimisticSchedulerLogic>,
    stats: Arc<Stats>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    clock: Arc<SteadyClock>,
    max_elections: usize,
    activation_delay: std::time::Duration,
}

impl OptimisticScheduler {
    pub fn new(
        params: OptimisticSchedulerParams,
        stats: Arc<Stats>,
        active_elections: Arc<RwLock<ActiveElectionsContainer>>,
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            thread: Mutex::new(None),
            stopped: AtomicBool::new(true),
            condition: Condvar::new(),
            max_elections: params.max_elections,
            activation_delay: params.activation_delay,
            logic: Mutex::new(OptimisticSchedulerLogic::new(params)),
            stats,
            active_elections,
            ledger,
            confirming_set,
            clock,
        }
    }

    pub fn max_elections(&self) -> usize {
        self.max_elections
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify();
        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
    }

    /// Notify about changes in AEC vacancy
    pub fn notify(&self) {
        self.condition.notify_all();
    }

    /// Called from backlog population to process accounts with unconfirmed blocks
    pub fn activate(
        &self,
        account: &Account,
        account_info: &AccountInfo,
        conf_info: &ConfirmationHeightInfo,
    ) -> bool {
        if self.stopped.load(Ordering::Relaxed) {
            return false;
        }
        let activated = self.logic.lock().unwrap().try_activate(
            account,
            account_info.block_count,
            conf_info.height,
        );
        if activated {
            self.stats
                .inc(StatType::OptimisticScheduler, DetailType::Activated);
        }
        activated
    }

    fn predicate(&self, logic: &OptimisticSchedulerLogic) -> bool {
        let active = self.active_elections.read().unwrap();
        let optimistic_count = active.count_by_behavior(ElectionBehavior::Optimistic);
        let aec_vacancy = active.vacancy();
        drop(active);
        logic.can_schedule(optimistic_count, aec_vacancy, self.activation_delay)
    }

    fn run(&self) {
        let mut guard = self.logic.lock().unwrap();
        while !self.stopped.load(Ordering::SeqCst) {
            self.stats
                .inc(StatType::OptimisticScheduler, DetailType::Loop);

            if self.predicate(&guard) {
                let any = self.ledger.any();

                while self.predicate(&guard) {
                    let (account, _) = guard.pop_candidate().unwrap();
                    drop(guard);
                    self.run_one(&any, account);
                    guard = self.logic.lock().unwrap();
                }
            }

            guard = self
                .condition
                .wait_timeout_while(guard, self.activation_delay / 2, |g| {
                    !self.stopped.load(Ordering::SeqCst) && !self.predicate(g)
                })
                .unwrap()
                .0;
        }
    }

    fn run_one(&self, any: &impl AnySet, account: Account) {
        let Some(head) = any.account_head(&account) else {
            return;
        };
        if let Some(block) = any.get_block(&head) {
            let forked = {
                #[cfg(not(feature = "ledger_snapshots"))]
                {
                    false
                }
                #[cfg(feature = "ledger_snapshots")]
                {
                    any.is_forked(&block.qualified_root())
                }
            };

            // Ensure block is not already confirmed
            let is_confirmed = self.confirming_set.contains(&block.hash())
                || any.confirmed().block_exists(&block.hash());

            if !is_confirmed && !forked {
                // Try to insert it into AEC
                // We check for AEC vacancy inside our predicate
                let now = self.clock.now();
                let priority = any.block_priority(&block);
                let inserted = self
                    .active_elections
                    .write()
                    .unwrap()
                    .insert(AecInsertRequest::new_optimistic(block, priority), now)
                    .is_ok();

                if inserted {
                    self.stats
                        .inc(StatType::OptimisticScheduler, DetailType::Insert);
                } else {
                    self.stats
                        .inc(StatType::OptimisticScheduler, DetailType::InsertFailed);
                }
            }
        }
    }
}

impl Drop for OptimisticScheduler {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        debug_assert!(self.thread.lock().unwrap().is_none())
    }
}

impl ContainerInfoProvider for OptimisticScheduler {
    fn container_info(&self) -> ContainerInfo {
        self.logic.lock().unwrap().container_info()
    }
}

pub trait OptimisticSchedulerExt {
    fn start(&self);
}

impl OptimisticSchedulerExt for Arc<OptimisticScheduler> {
    fn start(&self) {
        debug_assert!(self.thread.lock().unwrap().is_none());
        self.stopped.store(false, Ordering::SeqCst);
        let self_l = Arc::clone(self);
        *self.thread.lock().unwrap() = Some(
            std::thread::Builder::new()
                .name("Sched Opt".to_string())
                .spawn(Box::new(move || {
                    self_l.run();
                }))
                .unwrap(),
        );
    }
}
