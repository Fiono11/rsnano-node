use std::{any::Any, sync::Arc};

use rsnano_types::ConsensusEpoch;
use rsnano_utils::{CancellationToken, EventHandlerMut, thread_pool::ThreadPool, ticker::Tickable};

use super::{EpochDecisionService, ReportService};
use crate::consensus::{AecFact, AecService, AecTickerPlugin};

/// RAI, Section 6.1: the epoch this node just left is reported on. The
/// active elections announce the switch; the report is built from what this
/// node voted in the epoch and broadcast.
///
/// The report was frozen under the AEC lock at the boundary; signing and
/// sending it runs on a worker. On the AEC fact thread it held up every
/// cementation and confirmation behind it for up to a second per boundary.
pub(crate) struct ReportPlugin {
    reports: Arc<ReportService>,
    workers: Arc<ThreadPool>,
}

impl ReportPlugin {
    pub fn new(reports: Arc<ReportService>, workers: Arc<ThreadPool>) -> Self {
        Self { reports, workers }
    }
}

impl EventHandlerMut<AecFact> for ReportPlugin {
    fn handle(&mut self, event: &AecFact) {
        if let AecFact::EpochAdvanced(current, report) = event {
            let Some(left) = current.as_u64().checked_sub(1) else {
                return;
            };
            if let Some(report) = report {
                let reports = self.reports.clone();
                let report = report.clone();
                self.workers
                    .execute(move || reports.epoch_left(ConsensusEpoch::new(left), report));
            }
        }
    }
}

/// RAI: repeats the reconciliation requests whose answers did not come, and
/// drives the joint epoch election, on the AEC's tick
pub(crate) struct ReportTicker {
    reports: Arc<ReportService>,
    epoch_decision: Arc<EpochDecisionService>,
}

impl ReportTicker {
    pub fn new(reports: Arc<ReportService>, epoch_decision: Arc<EpochDecisionService>) -> Self {
        Self {
            reports,
            epoch_decision,
        }
    }
}

/// RAI: the report and close work runs on a timer thread of its own. On the
/// AEC ticker it held up election housekeeping and vote solicitation for
/// seconds while an epoch closed, which stalled the open epoch's account path.
impl Tickable for ReportTicker {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        self.run_once();
    }
}

impl ReportTicker {
    fn run_once(&mut self) {
        let started = std::time::Instant::now();
        let breakdown = self.reports.tick();
        let reports = started.elapsed();
        self.epoch_decision.tick();
        let decision = started.elapsed() - reports;
        if started.elapsed() >= std::time::Duration::from_millis(100) {
            crate::utils::diagnostic!(
                "SLOW_REPORT_TICK total_ms={} reports_ms={} decision_ms={} breakdown={:?}",
                started.elapsed().as_millis(),
                reports.as_millis(),
                decision.as_millis(),
                breakdown
            );
        }
    }
}

impl AecTickerPlugin for ReportTicker {
    fn run(&mut self, _aec: &AecService) {
        self.run_once();
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{
        EpochReport,
        election::{CertifiedState, ResidualVotes},
    };
    use rsnano_types::BlockHash;

    #[test]
    fn the_report_of_the_epoch_left_is_signed_on_a_worker() {
        let (mut plugin, workers) = create_plugin();

        plugin.handle(&AecFact::EpochAdvanced(
            ConsensusEpoch::new(1),
            Some(report()),
        ));

        assert_eq!(workers.queued_count(), 1);
        workers.simulate();
        assert_eq!(workers.queued_count(), 0);
    }

    #[test]
    fn nothing_is_signed_without_a_report() {
        let (mut plugin, workers) = create_plugin();

        plugin.handle(&AecFact::EpochAdvanced(ConsensusEpoch::new(1), None));
        plugin.handle(&AecFact::Recovered);

        assert_eq!(workers.queued_count(), 0);
    }

    /* Test helpers */

    fn create_plugin() -> (ReportPlugin, Arc<ThreadPool>) {
        let workers = Arc::new(ThreadPool::new_null());
        let plugin = ReportPlugin::new(Arc::new(ReportService::new_null()), workers.clone());
        (plugin, workers)
    }

    fn report() -> Arc<EpochReport> {
        Arc::new(EpochReport {
            committee: BlockHash::ZERO,
            certified: CertifiedState::new(),
            residual: ResidualVotes::new(),
        })
    }
}
