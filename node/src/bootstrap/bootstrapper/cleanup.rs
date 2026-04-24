use std::sync::Arc;

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::query_tracker::{QueryTracker, RunningQuery};
use crate::bootstrap::bootstrapper::bootstrap_queue::BootstrapQueue;

pub(super) struct BootstrapCleanup {
    clock: Arc<SteadyClock>,
    stats: Arc<Stats>,
    bootstrap_queue: Arc<BootstrapQueue>,
}

impl BootstrapCleanup {
    pub(super) fn new(
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
        bootstrap_queue: Arc<BootstrapQueue>,
    ) -> Self {
        Self {
            clock,
            stats,
            bootstrap_queue,
        }
    }

    pub fn cleanup(&mut self, query_tracker: &mut QueryTracker) {
        let now = self.clock.now();
        query_tracker.scoring.decay();
        self.erase_timed_out_requests(query_tracker, now);

        self.bootstrap_queue.timeout();
    }

    fn erase_timed_out_requests(&mut self, state: &mut QueryTracker, now: Timestamp) {
        let should_timeout = |query: &RunningQuery| query.response_cutoff < now;

        while let Some(front) = state.running_queries.front() {
            if !should_timeout(front) {
                break;
            }

            self.stats.inc(StatType::Bootstrap, DetailType::Timeout);
            self.stats
                .inc(StatType::BootstrapTimeout, front.query_type.into());
            state.running_queries.pop_front();
        }
    }

    pub fn reinsert_known_dependencies(&self) {
        self.bootstrap_queue.sync_dependencies();
    }
}
