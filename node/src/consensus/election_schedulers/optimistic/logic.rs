use std::{
    cmp::min,
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use rsnano_types::{Account, AccountInfo, ConfirmationHeightInfo};

use super::config::OptimisticSchedulerParams;

/// Pure scheduling logic — no infrastructure dependencies.
/// Manages the candidate queue and decides when activation is appropriate.
pub struct OptimisticSchedulerLogic {
    params: OptimisticSchedulerParams,
    candidates: OrderedCandidates,
}

impl OptimisticSchedulerLogic {
    pub fn new(params: OptimisticSchedulerParams) -> Self {
        Self {
            params,
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
            account_info.block_count - conf_info.height > self.params.gap_threshold;
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
        if self.candidates.len() >= self.params.max_size {
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
            self.params.max_elections as i64 - optimistic_count as i64,
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
        let logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(60); // gap = 40 > 32
        assert!(logic.activate_predicate(&account_info, &conf_info));
    }

    #[test]
    fn activate_predicate_gap_too_small() {
        let logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(80); // gap = 20 < 32
        assert!(!logic.activate_predicate(&account_info, &conf_info));
    }

    #[test]
    fn activate_predicate_nothing_confirmed_yet() {
        let logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        let account_info = make_account_info(5); // gap = 5, below threshold
        let conf_info = make_conf_info(0); // nothing confirmed
        assert!(logic.activate_predicate(&account_info, &conf_info));
    }

    #[test]
    fn try_activate_adds_candidate() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        let account = Account::from(1);
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(0);

        assert!(logic.try_activate(&account, &account_info, &conf_info));
        assert_eq!(logic.candidate_count(), 1);
    }

    #[test]
    fn try_activate_rejects_duplicate() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        let account = Account::from(1);
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(0);

        assert!(logic.try_activate(&account, &account_info, &conf_info));
        assert!(!logic.try_activate(&account, &account_info, &conf_info));
        assert_eq!(logic.candidate_count(), 1);
    }

    #[test]
    fn try_activate_rejects_when_full() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 2));
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(0);

        assert!(logic.try_activate(&Account::from(1), &account_info, &conf_info));
        assert!(logic.try_activate(&Account::from(2), &account_info, &conf_info));
        assert!(!logic.try_activate(&Account::from(3), &account_info, &conf_info));
        assert_eq!(logic.candidate_count(), 2);
    }

    #[test]
    fn try_activate_rejects_when_predicate_fails() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        let account_info = make_account_info(100);
        let conf_info = make_conf_info(80); // gap = 20, below threshold
        assert!(!logic.try_activate(&Account::from(1), &account_info, &conf_info));
        assert_eq!(logic.candidate_count(), 0);
    }

    #[test]
    fn can_schedule_no_candidates() {
        let logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        assert!(!logic.can_schedule(0, 10, Duration::ZERO));
    }

    #[test]
    fn can_schedule_no_vacancy() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
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
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        logic.try_activate(
            &Account::from(1),
            &make_account_info(100),
            &make_conf_info(0),
        );
        assert!(logic.can_schedule(0, 10, Duration::ZERO));
    }

    #[test]
    fn can_schedule_aec_vacancy_is_limiting_factor() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
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
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
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

    fn make_params(gap_threshold: u64, max_size: usize) -> OptimisticSchedulerParams {
        OptimisticSchedulerParams {
            gap_threshold,
            max_size,
            max_elections: 10,
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
