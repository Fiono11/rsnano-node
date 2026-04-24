use std::sync::Arc;

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::logic::{BootstrapLogic, RunningQuery};
use crate::bootstrap::bootstrapper::logic::BootstrapQueue;

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

    pub fn cleanup(&mut self, state: &mut BootstrapLogic) {
        let now = self.clock.now();
        self.stats.inc(StatType::Bootstrap, DetailType::LoopCleanup);
        state.scoring.decay();
        self.erase_timed_out_requests(state, now);
        self.bootstrap_queue.decay_blocked_accounts();
        self.bootstrap_queue.timeout();
    }

    fn erase_timed_out_requests(&mut self, state: &mut BootstrapLogic, now: Timestamp) {
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

    pub fn reinsert_known_dependencies(&mut self, state: &mut BootstrapLogic) {
        self.bootstrap_queue.sync_dependencies();
    }
}
