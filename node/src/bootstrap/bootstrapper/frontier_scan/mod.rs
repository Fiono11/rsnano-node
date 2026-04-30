mod coordinator;
mod database_crawler;
pub(crate) mod frontier_check_pool;
pub(crate) mod frontier_checker;
pub mod frontier_worker;
pub(crate) mod frontiers_processor;
pub(crate) mod stats;

use std::time::Duration;

use primitive_types::U512;

use rsnano_types::{Account, Frontier};

use crate::bootstrap::bootstrapper::{
    VerifyResult,
    query_tracker::{QueryType, RunningQuery},
};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrontierScanConfig {
    pub parallelism: usize,
    pub consideration_count: usize,
    pub candidates: usize,
    pub cooldown: Duration,
}

impl Default for FrontierScanConfig {
    fn default() -> Self {
        Self {
            parallelism: 128,
            consideration_count: 4,
            candidates: 1000,
            cooldown: Duration::from_secs(5),
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
pub struct FrontierHeadInfo {
    pub start: Account,
    pub end: Account,
    pub current: Account,
}

impl FrontierHeadInfo {
    /// Returns how far the current frontier is in the range [0, 1]
    pub fn done_normalized(&self) -> f32 {
        let total: U512 = (self.end.number() - self.start.number()).into();
        let mut progress: U512 = (self.current.number() - self.start.number()).into();
        progress *= 1000;
        (progress / total).as_u64() as f32 / 1000.0
    }
}

pub(crate) fn verify_frontiers(query: &RunningQuery, frontiers: &[Frontier]) -> VerifyResult {
    if query.query_type != QueryType::Frontiers {
        return VerifyResult::Invalid;
    }

    if frontiers.is_empty() {
        return VerifyResult::NothingNew;
    }

    if !are_accounts_in_ascending_order(frontiers) {
        return VerifyResult::Invalid;
    }

    // Ensure the frontiers are larger or equal to the requested frontier
    if frontiers[0].account.number() < query.start.number() {
        return VerifyResult::Invalid;
    }

    VerifyResult::Ok
}

fn are_accounts_in_ascending_order(frontiers: &[Frontier]) -> bool {
    let mut previous = &Account::ZERO;
    for f in frontiers {
        if f.account.number() <= previous.number() {
            return false;
        }
        previous = &f.account;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_frontiers() {
        let query = RunningQuery {
            query_type: QueryType::Frontiers,
            ..RunningQuery::new_test_instance()
        };

        assert_eq!(verify_frontiers(&query, &[]), VerifyResult::NothingNew);
    }

    #[test]
    fn valid_frontiers() {
        let query = RunningQuery {
            query_type: QueryType::Frontiers,
            start: 1.into(),
            ..RunningQuery::new_test_instance()
        };

        assert_eq!(
            verify_frontiers(
                &query,
                &[
                    Frontier::new(1.into(), 100.into()),
                    Frontier::new(2.into(), 200.into()),
                    Frontier::new(4.into(), 400.into()),
                ]
            ),
            VerifyResult::Ok
        );
    }

    /*
     * Error Cases
     */

    #[test]
    fn invalid_query_type() {
        let query = RunningQuery {
            query_type: QueryType::BlocksByHash, // invalid!
            ..RunningQuery::new_test_instance()
        };

        assert_eq!(verify_frontiers(&query, &[]), VerifyResult::Invalid);
    }

    #[test]
    fn accounts_not_in_order() {
        let query = RunningQuery {
            query_type: QueryType::Frontiers,
            start: 1.into(),
            ..RunningQuery::new_test_instance()
        };

        assert_eq!(
            verify_frontiers(
                &query,
                &[
                    Frontier::new(2.into(), 200.into()), // out of order!
                    Frontier::new(1.into(), 100.into()),
                    Frontier::new(4.into(), 400.into()),
                ]
            ),
            VerifyResult::Invalid
        );
    }

    #[test]
    fn accounts_lower_than_requested() {
        let query = RunningQuery {
            query_type: QueryType::Frontiers,
            start: 2.into(),
            ..RunningQuery::new_test_instance()
        };

        assert_eq!(
            verify_frontiers(
                &query,
                &[
                    Frontier::new(1.into(), 100.into()), // too low!
                    Frontier::new(2.into(), 200.into()),
                    Frontier::new(4.into(), 400.into()),
                ]
            ),
            VerifyResult::Invalid
        );
    }
}
