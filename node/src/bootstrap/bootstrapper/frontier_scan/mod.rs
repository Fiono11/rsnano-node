use crate::bootstrap::bootstrapper::{
    VerifyResult,
    query_tracker::{QueryType, RunningQuery},
};
use rsnano_types::{Account, Frontier};

pub(crate) mod coordinator;
pub(crate) mod frontiers_processor;
pub(crate) mod stats;

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
