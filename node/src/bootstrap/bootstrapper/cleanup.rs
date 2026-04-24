use std::sync::Arc;

use rsnano_nullable_clock::SteadyClock;

use super::query_tracker::QueryTracker;
use crate::bootstrap::bootstrapper::bootstrap_queue::BootstrapQueue;

pub(super) struct BootstrapCleanup {
    clock: Arc<SteadyClock>,
    bootstrap_queue: Arc<BootstrapQueue>,
    query_tracker: Arc<QueryTracker>,
}

impl BootstrapCleanup {
    pub(super) fn new(
        clock: Arc<SteadyClock>,
        bootstrap_queue: Arc<BootstrapQueue>,
        query_tracker: Arc<QueryTracker>,
    ) -> Self {
        Self {
            clock,
            bootstrap_queue,
            query_tracker,
        }
    }

    pub fn cleanup(&mut self) {
        let now = self.clock.now();
        self.query_tracker.timeout(now);
        self.bootstrap_queue.timeout();
    }

    pub fn reinsert_known_dependencies(&self) {
        self.bootstrap_queue.sync_dependencies();
    }
}
