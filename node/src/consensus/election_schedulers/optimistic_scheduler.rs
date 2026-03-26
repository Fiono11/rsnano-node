use std::{
    cmp::min,
    collections::{HashMap, VecDeque},
    mem::size_of,
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Account, AccountInfo, ConfirmationHeightInfo};
use rsnano_utils::{
    container_info::ContainerInfo,
    stats::{DetailType, StatType, Stats},
};

use crate::{
    cementation::ConfirmingSet,
    config::NetworkConstants,
    consensus::{ActiveElectionsContainer, AecInsertRequest, election::ElectionBehavior},
};

#[derive(Clone, Debug, PartialEq)]
pub struct OptimisticSchedulerConfig {
    /// Minimum difference between confirmation frontier and account frontier to become a candidate for optimistic confirmation
    pub gap_threshold: u64,

    /// Maximum number of candidates stored in memory
    pub max_size: usize,

    /// Limit of optimistic elections as percentage of `active_elections_size`
    pub optimistic_limit_percentage: usize,
}

impl OptimisticSchedulerConfig {
    pub fn new() -> Self {
        Self {
            gap_threshold: 32,
            max_size: 1024 * 64,
            optimistic_limit_percentage: 10,
        }
    }
}

impl Default for OptimisticSchedulerConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Pure scheduling logic — no infrastructure dependencies.
/// Manages the candidate queue and decides when activation is appropriate.
pub struct OptimisticSchedulerLogic {
    config: OptimisticSchedulerConfig,
    max_elections: usize,
    candidates: OrderedCandidates,
}

impl OptimisticSchedulerLogic {
    pub fn new(config: OptimisticSchedulerConfig, max_elections: usize) -> Self {
        Self {
            config,
            max_elections,
            candidates: OrderedCandidates::default(),
        }
    }

    /// Returns true if the account's unconfirmed gap meets the threshold for optimistic scheduling.
    pub fn activate_predicate(
        &self,
        account_info: &AccountInfo,
        conf_info: &ConfirmationHeightInfo,
    ) -> bool {
        let big_enough_gap =
            account_info.block_count - conf_info.height > self.config.gap_threshold;
        let nothing_confirmed_yet = conf_info.height == 0;
        big_enough_gap | nothing_confirmed_yet
    }

    /// Attempts to enqueue the account as an optimistic candidate.
    /// Returns true if the account was newly added.
    pub fn try_activate(
        &mut self,
        account: &Account,
        account_info: &AccountInfo,
        conf_info: &ConfirmationHeightInfo,
    ) -> bool {
        if !self.activate_predicate(account_info, conf_info) {
            return false;
        }
        if self.candidates.contains(account) {
            return false;
        }
        if self.candidates.len() >= self.config.max_size {
            return false;
        }
        self.candidates.insert(*account, Instant::now());
        true
    }

    /// Returns true if there is AEC vacancy and the front candidate has waited long enough.
    /// `optimistic_count` — current number of active optimistic elections.
    /// `aec_vacancy`      — total vacancy reported by the AEC.
    /// `activation_delay` — minimum time a candidate must wait before being scheduled.
    pub fn can_schedule(
        &self,
        optimistic_count: usize,
        aec_vacancy: i64,
        activation_delay: Duration,
    ) -> bool {
        let vacancy = min(
            self.max_elections as i64 - optimistic_count as i64,
            aec_vacancy,
        );
        if vacancy <= 0 {
            return false;
        }
        if let Some((_account, time)) = self.candidates.front() {
            time.elapsed() >= activation_delay
        } else {
            false
        }
    }

    pub fn pop_candidate(&mut self) -> Option<(Account, Instant)> {
        self.candidates.pop_front()
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }
}

pub struct OptimisticScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    stopped: AtomicBool,
    condition: Condvar,
    logic: Mutex<OptimisticSchedulerLogic>,
    stats: Arc<Stats>,
    active_elections: Arc<RwLock<ActiveElectionsContainer>>,
    network_constants: NetworkConstants,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    clock: Arc<SteadyClock>,
    pub max_elections: usize,
}

impl OptimisticScheduler {
    pub fn new(
        config: OptimisticSchedulerConfig,
        stats: Arc<Stats>,
        active_elections: Arc<RwLock<ActiveElectionsContainer>>,
        network_constants: NetworkConstants,
        ledger: Arc<Ledger>,
        confirming_set: Arc<ConfirmingSet>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        let max_elections =
            active_elections.read().unwrap().max_len() * config.optimistic_limit_percentage / 100;

        Self {
            thread: Mutex::new(None),
            stopped: AtomicBool::new(true),
            condition: Condvar::new(),
            logic: Mutex::new(OptimisticSchedulerLogic::new(config, max_elections)),
            stats,
            active_elections,
            network_constants,
            ledger,
            confirming_set,
            clock,
            max_elections,
        }
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
        let activated = self
            .logic
            .lock()
            .unwrap()
            .try_activate(account, account_info, conf_info);
        if activated {
            self.stats
                .inc(StatType::OptimisticScheduler, DetailType::Activated);
        }
        activated
    }

    pub fn container_info(&self) -> ContainerInfo {
        let guard = self.logic.lock().unwrap();
        [(
            "candidates",
            guard.candidate_count(),
            size_of::<Account>() * 2 + size_of::<Instant>(),
        )]
        .into()
    }

    fn predicate(&self, logic: &OptimisticSchedulerLogic) -> bool {
        let active = self.active_elections.read().unwrap();
        let optimistic_count = active.count_by_behavior(ElectionBehavior::Optimistic);
        let aec_vacancy = active.vacancy();
        drop(active);
        logic.can_schedule(
            optimistic_count,
            aec_vacancy,
            self.network_constants.optimistic_activation_delay,
        )
    }

    fn run(&self) {
        let mut guard = self.logic.lock().unwrap();
        while !self.stopped.load(Ordering::SeqCst) {
            self.stats
                .inc(StatType::OptimisticScheduler, DetailType::Loop);

            if self.predicate(&guard) {
                let any = self.ledger.any();

                while self.predicate(&guard) {
                    let (account, time) = guard.pop_candidate().unwrap();
                    drop(guard);
                    self.run_one(&any, account, time);
                    guard = self.logic.lock().unwrap();
                }
            }

            guard = self
                .condition
                .wait_timeout_while(
                    guard,
                    self.network_constants.optimistic_activation_delay / 2,
                    |g| !self.stopped.load(Ordering::SeqCst) && !self.predicate(g),
                )
                .unwrap()
                .0;
        }
    }

    fn run_one(&self, any: &impl AnySet, account: Account, _time: Instant) {
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

#[derive(Default)]
struct OrderedCandidates {
    by_account: HashMap<Account, Instant>,
    sequenced: VecDeque<Account>,
}

impl OrderedCandidates {
    fn insert(&mut self, account: Account, time: Instant) {
        if self.by_account.insert(account, time).is_some() {
            self.sequenced.retain(|i| *i != account);
        }
        self.sequenced.push_back(account);
    }

    fn len(&self) -> usize {
        self.sequenced.len()
    }

    fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    fn front(&self) -> Option<(Account, Instant)> {
        self.sequenced
            .front()
            .and_then(|account| self.by_account.get(account).map(|time| (*account, *time)))
    }

    fn pop_front(&mut self) -> Option<(Account, Instant)> {
        self.sequenced.pop_front().map(|account| {
            let time = self.by_account.remove(&account).unwrap();
            (account, time)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_predicate_gap_large_enough() {
        let logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(60); // gap = 40 > 32
        assert!(logic.activate_predicate(&account_info, &conf_info));
    }

    #[test]
    fn activate_predicate_gap_too_small() {
        let logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(80); // gap = 20 < 32
        assert!(!logic.activate_predicate(&account_info, &conf_info));
    }

    #[test]
    fn activate_predicate_nothing_confirmed_yet() {
        let logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        let account_info = make_account_info(5); // gap = 5, below threshold
        let conf_info = make_conf_info(0); // nothing confirmed
        assert!(logic.activate_predicate(&account_info, &conf_info));
    }

    #[test]
    fn try_activate_adds_candidate() {
        let mut logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        let account = Account::from(1);
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(0);

        assert!(logic.try_activate(&account, &account_info, &conf_info));
        assert_eq!(logic.candidate_count(), 1);
    }

    #[test]
    fn try_activate_rejects_duplicate() {
        let mut logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        let account = Account::from(1);
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(0);

        assert!(logic.try_activate(&account, &account_info, &conf_info));
        assert!(!logic.try_activate(&account, &account_info, &conf_info));
        assert_eq!(logic.candidate_count(), 1);
    }

    #[test]
    fn try_activate_rejects_when_full() {
        let mut logic = OptimisticSchedulerLogic::new(make_config(32, 2), 10);
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(0);

        assert!(logic.try_activate(&Account::from(1), &account_info, &conf_info));
        assert!(logic.try_activate(&Account::from(2), &account_info, &conf_info));
        assert!(!logic.try_activate(&Account::from(3), &account_info, &conf_info));
        assert_eq!(logic.candidate_count(), 2);
    }

    #[test]
    fn try_activate_rejects_when_predicate_fails() {
        let mut logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(80); // gap = 20, below threshold
        assert!(!logic.try_activate(&Account::from(1), &account_info, &conf_info));
        assert_eq!(logic.candidate_count(), 0);
    }

    #[test]
    fn can_schedule_no_candidates() {
        let logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        assert!(!logic.can_schedule(0, 10, Duration::ZERO));
    }

    #[test]
    fn can_schedule_no_vacancy() {
        let mut logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        logic.try_activate(
            &Account::from(1),
            &make_account_info(100),
            &make_conf_info(0),
        );
        // max_elections = 10, optimistic_count = 10 → vacancy = 0
        assert!(!logic.can_schedule(10, 10, Duration::ZERO));
    }

    #[test]
    fn can_schedule_with_vacancy_and_zero_delay() {
        let mut logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        logic.try_activate(
            &Account::from(1),
            &make_account_info(100),
            &make_conf_info(0),
        );
        assert!(logic.can_schedule(0, 10, Duration::ZERO));
    }

    #[test]
    fn can_schedule_aec_vacancy_is_limiting_factor() {
        let mut logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        logic.try_activate(
            &Account::from(1),
            &make_account_info(100),
            &make_conf_info(0),
        );
        // max_elections = 10, optimistic_count = 0 → 10 slots, but aec_vacancy = 0
        assert!(!logic.can_schedule(0, 0, Duration::ZERO));
    }

    #[test]
    fn pop_candidate_returns_in_insertion_order() {
        let mut logic = OptimisticSchedulerLogic::new(make_config(32, 1024), 10);
        let a = Account::from(1);
        let b = Account::from(2);
        logic.try_activate(&a, &make_account_info(100), &make_conf_info(0));
        logic.try_activate(&b, &make_account_info(100), &make_conf_info(0));

        let (first, _) = logic.pop_candidate().unwrap();
        assert_eq!(first, a);
        let (second, _) = logic.pop_candidate().unwrap();
        assert_eq!(second, b);
    }

    /* Test helpers */

    fn make_config(gap_threshold: u64, max_size: usize) -> OptimisticSchedulerConfig {
        OptimisticSchedulerConfig {
            gap_threshold,
            max_size,
            optimistic_limit_percentage: 10,
        }
    }

    fn make_account_info(block_count: u64) -> AccountInfo {
        AccountInfo {
            block_count,
            ..Default::default()
        }
    }

    fn make_conf_info(height: u64) -> ConfirmationHeightInfo {
        ConfirmationHeightInfo {
            height,
            ..Default::default()
        }
    }
}
