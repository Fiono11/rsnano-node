use std::{
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, RwLock},
    time::Duration,
};

use rsnano_ledger::{AnySet, Ledger, LedgerSet, OwningAnySet};
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::{Account, AccountInfo, ConfirmationHeightInfo};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
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
    logic: NullableCondvarMutex<OptimisticSchedulerLogic>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    clock: Arc<SteadyClock>,
    max_elections: usize,
    activation_delay: Duration,
    // stats
    loop_count: AtomicU64,
    activated_count: AtomicU64,
    insert_count: AtomicU64,
    insert_failed_count: AtomicU64,
}

impl OptimisticScheduler {
    pub fn new(
        params: OptimisticSchedulerParams,
        active_elections: Arc<RwLock<ActiveElectionsContainer>>,
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            max_elections: params.max_elections,
            activation_delay: params.activation_delay,
            logic: NullableCondvarMutex::new(OptimisticSchedulerLogic::new(params)),
            active_elections,
            ledger,
            confirming_set,
            clock,
            loop_count: AtomicU64::new(0),
            activated_count: AtomicU64::new(0),
            insert_count: AtomicU64::new(0),
            insert_failed_count: AtomicU64::new(0),
        }
    }

    pub fn max_elections(&self) -> usize {
        self.max_elections
    }

    pub fn stop(&self) {
        self.logic.lock().stop();
        self.logic.notify_all();
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
            self.activated_count.fetch_add(1, Ordering::Relaxed);
        }
        activated
    }

    pub fn run_loop(&self) {
        let mut logic = self.logic.lock();
        while !logic.stopped() {
            self.loop_count.fetch_add(1, Ordering::Relaxed);

            if self.can_schedule(&logic) {
                let any = self.ledger.any();

                while self.can_schedule(&logic) {
                    if let Some((account, _)) = logic.pop_candidate() {
                        drop(logic);
                        self.run_one(&any, account);
                        logic = self.logic.lock();
                    } else {
                        break;
                    }
                }
            }

            logic = self
                .logic
                .wait_timeout_while(logic, self.activation_delay / 2, |g| {
                    !g.stopped() && !self.can_schedule(g)
                })
                .0;
        }
    }

    fn run_one(&self, any: &OwningAnySet, account: Account) {
        let Some(head) = any.account_head(&account) else {
            return;
        };
        let Some(block) = any.get_block(&head) else {
            return;
        };

        #[cfg(feature = "ledger_snapshots")]
        {
            if any.is_forked(&block.qualified_root()) {
                // Needed for new consensus algorithm in ledger snapshot.
                // We never vote for forked blocks.
                return;
            }
        }

        // Ensure block is not already confirmed
        let is_confirmed = self.confirming_set.contains(&block.hash())
            || any.confirmed().block_exists(&block.hash());

        if is_confirmed {
            // No need to schedule an election if already confirmed.
            return;
        }
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
            self.insert_count.fetch_add(1, Ordering::Relaxed);
        } else {
            self.insert_failed_count.fetch_add(1, Ordering::Relaxed);
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
}

impl StatsSource for OptimisticScheduler {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "optimistic_scheduler",
            "loop",
            self.loop_count.load(Ordering::Relaxed),
        );
        result.insert(
            "optimistic_scheduler",
            "activated",
            self.activated_count.load(Ordering::Relaxed),
        );
        result.insert(
            "optimistic_scheduler",
            "insert",
            self.insert_count.load(Ordering::Relaxed),
        );
        result.insert(
            "optimistic_scheduler",
            "insert_failed",
            self.insert_failed_count.load(Ordering::Relaxed),
        );
    }
}

impl ContainerInfoProvider for OptimisticScheduler {
    fn container_info(&self) -> ContainerInfo {
        self.logic.lock().container_info()
    }
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
            Arc::new(RwLock::new(ActiveElectionsContainer::default())),
            Ledger::new_null().into(),
            ConfirmingSet::new_null().into(),
            SteadyClock::new_null().into(),
        )
    }
}
