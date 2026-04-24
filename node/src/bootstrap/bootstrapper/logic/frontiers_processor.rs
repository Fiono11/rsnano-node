use std::{collections::VecDeque, sync::Arc};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, Frontier};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

use crate::bootstrap::bootstrapper::{
    FrontierHeadInfo, FrontierScanConfig,
    frontier_scan::stats::FrontierScanStats,
    logic::{FrontierScan, RunningQuery, VerifyResult},
};

pub(crate) struct FrontiersProcessor {
    frontier_scan: FrontierScan,

    /// Frontiers that were received from other nodes and that we need to check against our ledger
    frontiers_to_check: VecDeque<Vec<Frontier>>,
    frontier_checker_overfill: bool,
    stats: Arc<FrontierScanStats>,
}

impl FrontiersProcessor {
    pub fn new(config: FrontierScanConfig, stats: Arc<FrontierScanStats>) -> Self {
        Self {
            frontier_scan: FrontierScan::new(config),
            frontiers_to_check: Default::default(),
            frontier_checker_overfill: false,
            stats,
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

    /// Returns true if the frontiers were valid
    pub(crate) fn process(
        &mut self,
        query: &RunningQuery,
        frontiers: Vec<Frontier>,
    ) -> VerifyResult {
        let result = query.verify_frontiers(&frontiers);
        if result == VerifyResult::Ok {
            self.frontier_scan.process(query.start.into(), &frontiers);
            self.frontiers_to_check.push_back(frontiers);
        };

        result
    }

    pub fn pop_frontiers_to_check(&mut self) -> Option<Vec<Frontier>> {
        self.frontiers_to_check.pop_front()
    }

    pub fn heads(&self) -> Vec<FrontierHeadInfo> {
        self.frontier_scan.heads()
    }
}

impl ContainerInfoProvider for FrontiersProcessor {
    fn container_info(&self) -> ContainerInfo {
        self.frontier_scan.container_info()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::bootstrapper::logic::{QuerySource, QueryType};

    #[test]
    fn empty_frontiers() {
        let stats = Arc::new(FrontierScanStats::default());
        let mut processor = FrontiersProcessor::new(Default::default(), stats);
        let query = running_query();

        let result = processor.process(&query, Vec::new());

        assert_eq!(result, VerifyResult::NothingNew);
    }

    #[test]
    fn update_account_ranges() {
        let stats = Arc::new(FrontierScanStats::default());
        let mut processor = FrontiersProcessor::new(Default::default(), stats);
        let query = running_query();

        let result = processor.process(&query, vec![Frontier::new_test_instance()]);

        assert_eq!(result, VerifyResult::Ok);
        assert_eq!(processor.frontier_scan.total_requests_completed(), 1);
    }

    #[test]
    fn invalid_frontiers() {
        let stats = Arc::new(FrontierScanStats::default());
        let mut processor = FrontiersProcessor::new(Default::default(), stats);
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
