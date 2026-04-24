use std::sync::{Arc, Mutex};

use rsnano_ledger::Ledger;
use rsnano_utils::{stats::Stats, thread_pool::ThreadPool};

use crate::bootstrap::bootstrapper::{
    bootstrap_queue::BootstrapQueue, logic::BootstrapLogic,
    response_processor::frontier_worker::FrontierWorker,
};

pub(crate) struct FrontierCheckPool {
    stats: Arc<Stats>,
    ledger: Arc<Ledger>,
    logic: Arc<Mutex<BootstrapLogic>>,
    workers: Arc<ThreadPool>,
    bootstrap_queue: Arc<BootstrapQueue>,
    pub max_pending: usize,
}

impl FrontierCheckPool {
    pub(crate) fn new(
        stats: Arc<Stats>,
        ledger: Arc<Ledger>,
        state: Arc<Mutex<BootstrapLogic>>,
        bootstrap_queue: Arc<BootstrapQueue>,
    ) -> Self {
        let workers = Arc::new(ThreadPool::new(1, "Bootstrap work"));
        Self {
            stats,
            ledger,
            logic: state,
            workers,
            bootstrap_queue,
            max_pending: 16,
        }
    }

    pub(crate) fn enqueue_frontiers(&self, logic: &mut BootstrapLogic) {
        while let Some(frontiers) = logic.frontiers_processor.pop_frontiers_to_check() {
            let ledger = self.ledger.clone();
            let stats = self.stats.clone();
            let state = self.logic.clone();
            let bootstrap_queue = self.bootstrap_queue.clone();
            self.workers.execute(move || {
                let any = ledger.any();
                let mut worker = FrontierWorker::new(&any, &stats, &state, &bootstrap_queue);
                worker.process(frontiers);
            });
        }
        let queued_tasks = self.workers.queued_count();
        logic
            .frontiers_processor
            .set_frontier_checker_overfill(queued_tasks >= self.max_pending);
    }
}
