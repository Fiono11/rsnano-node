use std::sync::{Arc, atomic::Ordering::Relaxed};

use rsnano_ledger::Ledger;
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{Stats, StatsCollection, StatsSource},
    thread_pool::ThreadPool,
};

use super::{
    frontier_worker::FrontierWorker, frontiers_processor::FrontiersProcessor,
    stats::FrontierScanStats,
};
use crate::bootstrap::bootstrapper::{
    FrontierScanSnapshot, VerifyResult,
    bootstrap_queue::{self, BootstrapQueue},
    frontier_scan::frontiers_processor,
    query_tracker::RunningQuery,
};
use rsnano_types::{Account, Frontier};

pub(crate) struct FrontierCheckPool {
    stats: Arc<Stats>,
    stats2: Arc<FrontierScanStats>,
    ledger: Arc<Ledger>,
    workers: Arc<ThreadPool>,
    frontiers_processor: Arc<FrontiersProcessor>,
    bootstrap_queue: Arc<BootstrapQueue>,
    pub max_pending: usize,
}

impl FrontierCheckPool {
    pub fn new(
        stats: Arc<Stats>,
        ledger: Arc<Ledger>,
        bootstrap_queue: Arc<BootstrapQueue>,
        frontiers_processor: Arc<FrontiersProcessor>,
    ) -> Self {
        let workers = Arc::new(ThreadPool::new(1, "Bootstrap work"));
        let stats2 = Arc::new(FrontierScanStats::default());
        Self {
            stats,
            stats2,
            ledger,
            workers,
            bootstrap_queue,
            frontiers_processor,
            max_pending: 16,
        }
    }

    #[cfg(test)]
    pub fn new_null() -> Self {
        let stats = Arc::new(Stats::default());
        let ledger = Arc::new(Ledger::new_null());
        let bootstrap_queue = Arc::new(BootstrapQueue::new_null());
        let frontiers_processor = Arc::new(FrontiersProcessor::new_null());
        Self::new(stats, ledger, bootstrap_queue, frontiers_processor)
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

impl ContainerInfoProvider for FrontierCheckPool {
    fn container_info(&self) -> ContainerInfo {
        self.frontiers_processor.container_info()
    }
}

impl StatsSource for FrontierCheckPool {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats2.collect_stats(result);
    }
}
