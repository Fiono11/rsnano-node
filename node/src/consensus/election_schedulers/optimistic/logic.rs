use std::cmp::min;

use rsnano_nullable_clock::Timestamp;
use rsnano_types::Account;

use super::{candidate_queue::CandidateQueue, config::OptimisticSchedulerParams};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

/// Pure scheduling logic — no infrastructure dependencies.
/// Manages the candidate queue and decides when activation is appropriate.
pub struct OptimisticSchedulerLogic {
    stopped: bool,
    params: OptimisticSchedulerParams,
    candidates: CandidateQueue,
}

impl OptimisticSchedulerLogic {
    pub fn new(params: OptimisticSchedulerParams) -> Self {
        Self {
            stopped: false,
            params,
            candidates: CandidateQueue::default(),
        }
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }

    pub fn stop(&mut self) {
        self.stopped = true;
    }

    /// Returns true if the account's unconfirmed gap meets the threshold for optimistic scheduling.
    pub fn has_eligible_gap(&self, block_count: u64, confirmation_height: u64) -> bool {
        let big_enough_gap = block_count - confirmation_height > self.params.gap_threshold;
        let nothing_confirmed_yet = confirmation_height == 0;
        big_enough_gap | nothing_confirmed_yet
    }

    /// Attempts to enqueue the account as an optimistic candidate.
    /// Returns true if the account was newly added.
    pub fn try_activate(
        &mut self,
        account: &Account,
        block_count: u64,
        confirmation_height: u64,
        now: Timestamp,
    ) -> bool {
        if self.stopped() {
            return false;
        }
        if !self.has_eligible_gap(block_count, confirmation_height) {
            return false;
        }
        if self.candidates.contains(account) {
            return false;
        }
        if self.candidates.len() >= self.params.max_candidates {
            return false;
        }
        self.candidates.insert(*account, now);
        true
    }

    /// Returns true if there is AEC vacancy and the front candidate has waited long enough.
    /// `optimistic_count` — current number of active optimistic elections.
    /// `aec_vacancy`      — total vacancy reported by the AEC.
    /// `activation_delay` — minimum time a candidate must wait before being scheduled.
    pub fn can_schedule(&self, optimistic_count: usize, aec_vacancy: i64, now: Timestamp) -> bool {
        let vacancy = min(
            self.params.max_elections as i64 - optimistic_count as i64,
            aec_vacancy,
        );
        if vacancy <= 0 {
            return false;
        }
        if let Some((_, time)) = self.candidates.front() {
            time.elapsed(now) >= self.params.activation_delay
        } else {
            false
        }
    }

    pub fn pop_candidate(&mut self) -> Option<Account> {
        self.candidates.pop_front().map(|(account, _)| account)
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }
}

impl ContainerInfoProvider for OptimisticSchedulerLogic {
    fn container_info(&self) -> ContainerInfo {
        [(
            "candidates",
            self.candidate_count(),
            size_of::<Account>() * 2 + size_of::<Timestamp>(),
        )]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stopped_initially_false() {
        assert!(!OptimisticSchedulerLogic::new(make_params(32, 1024)).stopped());
    }

    #[test]
    fn stop_sets_flag() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        logic.stop();
        assert!(logic.stopped());
    }

    #[test]
    fn eligible_gap_when_gap_large_enough() {
        let logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        assert!(logic.has_eligible_gap(100, 60)); // gap = 40 > 32
    }

    #[test]
    fn not_eligible_when_gap_too_small() {
        let logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        assert!(!logic.has_eligible_gap(100, 80)); // gap = 20 < 32
    }

    #[test]
    fn eligible_gap_when_nothing_confirmed_yet() {
        let logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        assert!(logic.has_eligible_gap(5, 0)); // gap = 5, below threshold but nothing confirmed
    }

    #[test]
    fn try_activate_adds_candidate() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        assert!(logic.try_activate(&Account::from(1), 100, 0, now()));
        assert_eq!(logic.candidate_count(), 1);
    }

    #[test]
    fn try_activate_rejects_duplicate() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        let account = Account::from(1);
        assert!(logic.try_activate(&account, 100, 0, now()));
        assert!(!logic.try_activate(&account, 100, 0, now()));
        assert_eq!(logic.candidate_count(), 1);
    }

    #[test]
    fn try_activate_rejects_when_full() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 2));
        assert!(logic.try_activate(&Account::from(1), 100, 0, now()));
        assert!(logic.try_activate(&Account::from(2), 100, 0, now()));
        assert!(!logic.try_activate(&Account::from(3), 100, 0, now()));
        assert_eq!(logic.candidate_count(), 2);
    }

    #[test]
    fn try_activate_rejects_when_gap_too_small() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        assert!(!logic.try_activate(&Account::from(1), 100, 80, now())); // gap = 20, below threshold
        assert_eq!(logic.candidate_count(), 0);
    }

    #[test]
    fn can_schedule_no_candidates() {
        let logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        assert!(!logic.can_schedule(0, 10, now()));
    }

    #[test]
    fn can_schedule_no_vacancy() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        logic.try_activate(&Account::from(1), 100, 0, now());
        // max_elections = 10, optimistic_count = 10 → vacancy = 0
        assert!(!logic.can_schedule(10, 10, now()));
    }

    #[test]
    fn can_schedule_with_vacancy_and_zero_delay() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        logic.try_activate(&Account::from(1), 100, 0, now());
        assert!(logic.can_schedule(0, 10, now()));
    }

    #[test]
    fn can_schedule_aec_vacancy_is_limiting_factor() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        logic.try_activate(&Account::from(1), 100, 0, now());
        // max_elections = 10, optimistic_count = 0 → 10 slots, but aec_vacancy = 0
        assert!(!logic.can_schedule(0, 0, now()));
    }

    #[test]
    fn pop_candidate_returns_in_insertion_order() {
        let mut logic = OptimisticSchedulerLogic::new(make_params(32, 1024));
        let a = Account::from(1);
        let b = Account::from(2);
        logic.try_activate(&a, 100, 0, now());
        logic.try_activate(&b, 100, 0, now());

        let first = logic.pop_candidate().unwrap();
        assert_eq!(first, a);
        let second = logic.pop_candidate().unwrap();
        assert_eq!(second, b);
    }

    /* Test helpers */

    fn now() -> Timestamp {
        Timestamp::new_test_instance()
    }

    fn make_params(gap_threshold: u64, max_candidates: usize) -> OptimisticSchedulerParams {
        OptimisticSchedulerParams {
            gap_threshold,
            max_candidates,
            max_elections: 10,
            activation_delay: Duration::ZERO,
        }
    }
}
