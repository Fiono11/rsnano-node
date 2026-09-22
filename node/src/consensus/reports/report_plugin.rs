use std::{any::Any, sync::Arc};

use rsnano_types::ConsensusEpoch;
use rsnano_utils::EventHandlerMut;

use super::ReportService;
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
        if let AecFact::EpochAdvanced(current) = event {
            let Some(left) = current.as_u64().checked_sub(1) else {
                return;
            };
            self.reports.epoch_left(ConsensusEpoch::new(left));
        }
    }
}

/// RAI, Section 6.2: repeats the reconciliation requests whose answers did
/// not come, on the AEC's tick
pub(crate) struct ReportTicker {
    reports: Arc<ReportService>,
}

impl ReportTicker {
    pub fn new(reports: Arc<ReportService>) -> Self {
        Self { reports }
    }
}

impl AecTickerPlugin for ReportTicker {
    fn run(&mut self, _aec: &AecService) {
        self.reports.tick();
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
