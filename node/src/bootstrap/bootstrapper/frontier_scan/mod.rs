mod coordinator;
mod database_crawler;
mod frontier_checker;
mod frontier_worker;
mod frontiers_processor;
mod stats;

use std::{
    sync::{Arc, atomic::Ordering::Relaxed},
    time::Duration,
};

use primitive_types::U512;

use rsnano_ledger::Ledger;
use rsnano_types::{Account, Frontier};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{Stats, StatsCollection, StatsSource},
    thread_pool::ThreadPool,
};

use crate::bootstrap::bootstrapper::{
    FrontierScanSnapshot, VerifyResult,
    bootstrap_queue::BootstrapQueue,
    query_tracker::{QueryType, RunningQuery},
};
use frontier_worker::FrontierWorker;
use frontiers_processor::FrontiersProcessor;
use stats::FrontierScanStats;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrontierScanConfig {
    pub parallelism: usize,
    pub consideration_count: usize,
    pub candidates: usize,
    pub cooldown: Duration,
    /// How many frontier acks can get queued in the processor
    pub max_pending_frontier_responses: usize,
}

impl Default for FrontierScanConfig {
    fn default() -> Self {
        Self {
            parallelism: 128,
            consideration_count: 4,
            candidates: 1000,
            cooldown: Duration::from_secs(5),
            max_pending_frontier_responses: 16,
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

pub(crate) struct FrontierScan {
    stats: Arc<Stats>,
    stats2: Arc<FrontierScanStats>,
    ledger: Arc<Ledger>,
    workers: Arc<ThreadPool>,
    frontiers_processor: Arc<FrontiersProcessor>,
    bootstrap_queue: Arc<BootstrapQueue>,
    max_pending: usize,
}

impl FrontierScan {
    pub fn new(
        stats: Arc<Stats>,
        ledger: Arc<Ledger>,
        bootstrap_queue: Arc<BootstrapQueue>,
        config: FrontierScanConfig,
    ) -> Self {
        let workers = Arc::new(ThreadPool::new(1, "Bootstrap work"));
        let max_pending = config.max_pending_frontier_responses;
        let frontiers_processor = Arc::new(FrontiersProcessor::new(config));
        let stats2 = Arc::new(FrontierScanStats::default());
        Self {
            stats,
            stats2,
            ledger,
            workers,
            bootstrap_queue,
            frontiers_processor,
            max_pending,
        }
    }

    #[cfg(test)]
    pub fn new_null() -> Self {
        let stats = Arc::new(Stats::default());
        let ledger = Arc::new(Ledger::new_null());
        let bootstrap_queue = Arc::new(BootstrapQueue::new_null());
        let frontiers_processor = Arc::new(FrontiersProcessor::new_null());
        let stats2 = Arc::new(FrontierScanStats::default());
        let workers = Arc::new(ThreadPool::new_null());
        let max_pending = 16;
        Self {
            stats,
            stats2,
            ledger,
            workers,
            bootstrap_queue,
            frontiers_processor,
            max_pending,
        }
    }

    pub fn frontier_checker_overfill(&self) -> bool {
        self.frontiers_processor.frontier_checker_overfill()
    }

    pub fn next_account_to_query(&self) -> Account {
        self.frontiers_processor.next_account_to_query()
    }

    pub fn process(&self, query: &RunningQuery, frontiers: Vec<Frontier>) -> bool {
        match self.frontiers_processor.process(query, frontiers) {
            VerifyResult::Ok => {
                self.stats2.verified.fetch_add(1, Relaxed);
                true
            }
            VerifyResult::NothingNew => {
                self.stats2.nothing_new.fetch_add(1, Relaxed);
                true
            }
            VerifyResult::Invalid => {
                self.stats2.invalid.fetch_add(1, Relaxed);
                false
            }
        }
    }

    pub fn enqueue_frontiers(&self) {
        while let Some(frontiers) = self.frontiers_processor.pop_received_frontiers() {
            let ledger = self.ledger.clone();
            let stats = self.stats.clone();
            let stats2 = self.stats2.clone();
            let bootstrap_queue = self.bootstrap_queue.clone();
            self.workers.execute(move || {
                let any = ledger.any();
                let mut worker = FrontierWorker::new(&any, &stats, &stats2, &bootstrap_queue);
                worker.process(frontiers);
            });
        }
        let queued_tasks = self.workers.queued_count();
        self.frontiers_processor
            .set_frontier_checker_overfill(queued_tasks >= self.max_pending);
    }

    pub fn snapshot(&self) -> FrontierScanSnapshot {
        FrontierScanSnapshot {
            processed_frontiers: self.stats2.processed_frontiers.load(Relaxed),
            outdated_accounts_found: self.stats2.outdated_accounts_found.load(Relaxed),
            heads: self.frontiers_processor.heads(),
            last_outdated_accounts: self.stats2.last_outdated_found(),
        }
    }
}

impl ContainerInfoProvider for FrontierScan {
    fn container_info(&self) -> ContainerInfo {
        self.frontiers_processor.container_info()
    }
}

impl StatsSource for FrontierScan {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats2.collect_stats(result);
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
