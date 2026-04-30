use std::{collections::VecDeque, sync::Mutex};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{Account, Frontier};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

use super::{VerifyResult, coordinator::FrontierScanCoordinator, verify_frontiers};
use crate::bootstrap::bootstrapper::{
    FrontierHeadInfo, FrontierScanConfig, query_tracker::RunningQuery,
};

pub(crate) struct FrontiersProcessor {
    logic: Mutex<FrontiersProcessorLogic>,
    clock: SteadyClock,
}

impl FrontiersProcessor {
    pub fn new(config: FrontierScanConfig) -> Self {
        Self::new_impl(config, SteadyClock::default())
    }

    #[cfg(test)]
    pub fn new_null() -> Self {
        Self::new_impl(Default::default(), SteadyClock::new_null())
    }

    fn new_impl(config: FrontierScanConfig, clock: SteadyClock) -> Self {
        Self {
            logic: Mutex::new(FrontiersProcessorLogic::new(config)),
            clock,
        }
    }

    pub fn set_frontier_checker_overfill(&self, overfill: bool) {
        self.logic
            .lock()
            .unwrap()
            .set_frontier_checker_overfill(overfill)
    }

    pub fn frontier_checker_overfill(&self) -> bool {
        self.logic.lock().unwrap().frontier_checker_overfill()
    }

    pub fn next_account_to_query(&self) -> Account {
        let now = self.clock.now();
        self.logic.lock().unwrap().next(now)
    }

    pub(crate) fn process(&self, query: &RunningQuery, frontiers: Vec<Frontier>) -> VerifyResult {
        self.logic.lock().unwrap().process(query, frontiers)
    }

    pub fn pop_received_frontiers(&self) -> Option<Vec<Frontier>> {
        self.logic.lock().unwrap().pop_received_frontiers()
    }

    pub fn heads(&self) -> Vec<FrontierHeadInfo> {
        self.logic.lock().unwrap().heads()
    }
}

impl ContainerInfoProvider for FrontiersProcessor {
    fn container_info(&self) -> ContainerInfo {
        self.logic.lock().unwrap().container_info()
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct OutdatedAccounts {
    pub accounts: Vec<Account>,
    /// Accounts that exist but are outdated
    pub outdated: usize,
    /// Accounts that don't exist but have pending blocks in the ledger
    pub pending: usize,
    /// Total count of received frontiers
    pub frontiers_received: usize,
}

struct FrontiersProcessorLogic {
    frontier_scan: FrontierScanCoordinator,

    /// Frontiers that were received from other nodes and that we need to check against our ledger
    received_frontiers: VecDeque<Vec<Frontier>>,
    frontier_checker_overfill: bool,
}

impl FrontiersProcessorLogic {
    pub fn new(config: FrontierScanConfig) -> Self {
        Self {
            frontier_scan: FrontierScanCoordinator::new(config),
            received_frontiers: Default::default(),
            frontier_checker_overfill: false,
        }
    }

    pub fn set_frontier_checker_overfill(&mut self, overfill: bool) {
        self.frontier_checker_overfill = overfill;
    }

    pub fn frontier_checker_overfill(&self) -> bool {
        self.frontier_checker_overfill
    }

    pub fn next(&mut self, now: Timestamp) -> Account {
        self.frontier_scan.next(now)
    }

    pub(crate) fn process(
        &mut self,
        query: &RunningQuery,
        frontiers: Vec<Frontier>,
    ) -> VerifyResult {
        let result = verify_frontiers(query, &frontiers);
        if result == VerifyResult::Ok {
            self.frontier_scan.process(query.start.into(), &frontiers);
            self.received_frontiers.push_back(frontiers);
        };

        result
    }

    pub fn pop_received_frontiers(&mut self) -> Option<Vec<Frontier>> {
        self.received_frontiers.pop_front()
    }

    pub fn heads(&self) -> Vec<FrontierHeadInfo> {
        self.frontier_scan.heads()
    }
}

impl ContainerInfoProvider for FrontiersProcessorLogic {
    fn container_info(&self) -> ContainerInfo {
        self.frontier_scan.container_info()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::bootstrapper::query_tracker::{QuerySource, QueryType};

    #[test]
    fn empty_frontiers() {
        let mut processor = FrontiersProcessorLogic::new(Default::default());
        let query = running_query();

        let result = processor.process(&query, Vec::new());

        assert_eq!(result, VerifyResult::NothingNew);
    }

    #[test]
    fn update_account_ranges() {
        let mut processor = FrontiersProcessorLogic::new(Default::default());
        let query = running_query();

        let result = processor.process(&query, vec![Frontier::new_test_instance()]);

        assert_eq!(result, VerifyResult::Ok);
        assert_eq!(processor.frontier_scan.total_requests_completed(), 1);
    }

    #[test]
    fn invalid_frontiers() {
        let mut processor = FrontiersProcessorLogic::new(Default::default());
        let query = running_query();

        let frontiers = vec![
            Frontier::new(3.into(), 100.into()),
            Frontier::new(1.into(), 200.into()), // descending order is invalid!
        ];

        let result = processor.process(&query, frontiers);

        assert_eq!(result, VerifyResult::Invalid);
        assert_eq!(processor.frontier_scan.total_requests_completed(), 0);
    }

    fn running_query() -> RunningQuery {
        RunningQuery {
            source: QuerySource::Frontiers,
            query_type: QueryType::Frontiers,
            start: 1.into(),
            ..RunningQuery::new_test_instance()
        }
    }
}
