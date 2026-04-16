use std::sync::Arc;

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::logic::{BootstrapLogic, RunningQuery};

pub(super) struct BootstrapCleanup {
    clock: Arc<SteadyClock>,
    stats: Arc<Stats>,
}

impl BootstrapCleanup {
    pub(super) fn new(clock: Arc<SteadyClock>, stats: Arc<Stats>) -> Self {
        Self { clock, stats }
    }

    pub fn cleanup(&mut self, state: &mut BootstrapLogic) {
        let now = self.clock.now();
        self.stats.inc(StatType::Bootstrap, DetailType::LoopCleanup);
        state.scoring.decay();

        let decayed = state.bootstrap_queue.decay_blocked_accounts(now);
        self.stats.add(
            StatType::BootstrapAccountSets,
            DetailType::BlockingDecayed,
            decayed as u64,
        );

        self.erase_timed_out_requests(state, now);
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
        state.bootstrap_queue.timeout(now);
    }

    pub fn reinsert_known_dependencies(&mut self, state: &mut BootstrapLogic) {
        self.stats
            .inc(StatType::Bootstrap, DetailType::SyncDependencies);

        let inserted = state.bootstrap_queue.sync_dependencies();

        if inserted > 0 {
            self.stats.add(
                StatType::BootstrapAccountSets,
                DetailType::PriorityInsert,
                inserted as u64,
            );
            self.stats.add(
                StatType::BootstrapAccountSets,
                DetailType::DependencySynced,
                inserted as u64,
            );
        }
    }
}
