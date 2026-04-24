use std::sync::{Arc, Mutex};

use rsnano_ledger::Ledger;
use rsnano_utils::{stats::Stats, thread_pool::ThreadPool};

use crate::bootstrap::bootstrapper::{
    bootstrap_queue::BootstrapQueue, frontier_scan::stats::FrontierScanStats,
    logic::BootstrapLogic, response_processor::frontier_worker::FrontierWorker,
};

pub(crate) struct FrontierCheckPool {
    stats: Arc<Stats>,
    stats2: Arc<FrontierScanStats>,
    ledger: Arc<Ledger>,
    workers: Arc<ThreadPool>,
    bootstrap_queue: Arc<BootstrapQueue>,
    pub max_pending: usize,
}

impl FrontierCheckPool {
    pub(crate) fn new(
        stats: Arc<Stats>,
        stats2: Arc<FrontierScanStats>,
        ledger: Arc<Ledger>,
        bootstrap_queue: Arc<BootstrapQueue>,
    ) -> Self {
        let workers = Arc::new(ThreadPool::new(1, "Bootstrap work"));
        Self {
            stats,
            stats2,
            ledger,
            workers,
            bootstrap_queue,
            max_pending: 16,
        }
    }

    pub(crate) fn enqueue_frontiers(&self, logic: &mut BootstrapLogic) {
        while let Some(frontiers) = logic.frontiers_processor.pop_frontiers_to_check() {
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
        logic
            .frontiers_processor
            .set_frontier_checker_overfill(queued_tasks >= self.max_pending);
    }
}
