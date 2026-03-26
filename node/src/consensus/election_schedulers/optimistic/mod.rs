use std::{
    sync::{Arc, Mutex, RwLock},
    thread::JoinHandle,
    time::Duration,
};

use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_condvar::NullableCondvarMutex;
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
    logic: NullableCondvarMutex<OptimisticSchedulerLogic>,
    stats: Arc<Stats>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    clock: Arc<SteadyClock>,
    max_elections: usize,
    activation_delay: Duration,
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
            max_elections: params.max_elections,
            activation_delay: params.activation_delay,
            logic: NullableCondvarMutex::new(OptimisticSchedulerLogic::new(params)),
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
        self.logic.lock().stop();
        self.logic.notify_all();
        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
    }

    /// Notify about changes in AEC vacancy
    pub fn notify(&self) {
        self.logic.notify_all();
    }

    /// Called from backlog population to process accounts with unconfirmed blocks
    pub fn activate(
        &self,
        account: &Account,
        account_info: &AccountInfo,
        conf_info: &ConfirmationHeightInfo,
    ) -> bool {
        let mut logic = self.logic.lock();
        if logic.stopped() {
            return false;
        }
        let activated = logic.try_activate(
            account,
            account_info.block_count,
            conf_info.height,
            self.clock.now(),
        );
        if activated {
            self.stats
                .inc(StatType::OptimisticScheduler, DetailType::Activated);
        }
        activated
    }

    fn run(&self) {
        let mut guard = self.logic.lock();
        while !guard.stopped() {
            self.stats
                .inc(StatType::OptimisticScheduler, DetailType::Loop);

            if self.can_schedule(&guard) {
                let any = self.ledger.any();

                while self.can_schedule(&guard) {
                    let (account, _) = guard.pop_candidate().unwrap();
                    drop(guard);
                    self.run_one(&any, account);
                    guard = self.logic.lock();
                }
            }

            guard = self
                .logic
                .wait_timeout_while(guard, self.activation_delay / 2, |g| {
                    !g.stopped() && !self.can_schedule(g)
                })
                .0;
        }
    }

    fn can_schedule(&self, logic: &OptimisticSchedulerLogic) -> bool {
        let optimistic_count;
        let aec_vacancy;
        {
            let aec = self.active_elections.read().unwrap();
            optimistic_count = aec.count_by_behavior(ElectionBehavior::Optimistic);
            aec_vacancy = aec.vacancy();
        }
        logic.can_schedule(optimistic_count, aec_vacancy, self.clock.now())
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
        self.logic.lock().container_info()
    }
}

pub trait OptimisticSchedulerExt {
    fn start(&self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_condvar::NotifyEvent;

    #[test]
    fn stop_sets_stopped_flag_and_notifies() {
        let scheduler = make_scheduler();
        let tracker = scheduler.logic.track_notifications();

        scheduler.stop();

        assert!(scheduler.logic.lock().stopped());
        assert_eq!(tracker.output(), vec![NotifyEvent::NotifyAll]);
    }

    #[test]
    fn notify_sends_notify_all() {
        let scheduler = make_scheduler();
        let tracker = scheduler.logic.track_notifications();

        scheduler.notify();

        assert_eq!(tracker.output(), vec![NotifyEvent::NotifyAll]);
    }

    /* Test helpers */

    fn make_scheduler() -> OptimisticScheduler {
        OptimisticScheduler::new(
            OptimisticSchedulerParams {
                gap_threshold: 32,
                max_candidates: 1024,
                max_elections: 10,
                activation_delay: Duration::ZERO,
            },
            Arc::new(Stats::default()),
            Arc::new(RwLock::new(ActiveElectionsContainer::default())),
            Ledger::new_null().into(),
            ConfirmingSet::new_null().into(),
            SteadyClock::new_null().into(),
        )
    }
}

impl OptimisticSchedulerExt for Arc<OptimisticScheduler> {
    fn start(&self) {
        debug_assert!(self.thread.lock().unwrap().is_none());
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
