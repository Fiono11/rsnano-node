use std::{any::Any, sync::Arc};

use rsnano_types::ConsensusEpoch;
use rsnano_utils::{CancellationToken, EventHandlerMut, ticker::Tickable};

use super::{EpochDecisionService, ReportService};
use crate::consensus::{AecFact, AecService, AecTickerPlugin};

/// RAI, Section 6.1: the epoch this node just left is reported on. The
/// active elections announce the switch; the report is built from what this
/// node voted in the epoch and broadcast.
pub(crate) struct ReportPlugin {
    reports: Arc<ReportService>,
}

impl ReportPlugin {
    pub fn new(reports: Arc<ReportService>) -> Self {
        Self { reports }
    }
}

impl EventHandlerMut<AecFact> for ReportPlugin {
    fn handle(&mut self, event: &AecFact) {
        if let AecFact::EpochAdvanced(current, report) = event {
            let Some(left) = current.as_u64().checked_sub(1) else {
                return;
            };
            if let Some(report) = report {
                self.reports
                    .epoch_left(ConsensusEpoch::new(left), report.clone());
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
