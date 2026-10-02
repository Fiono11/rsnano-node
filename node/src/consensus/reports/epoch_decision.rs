use super::ReportExchange;
use crate::consensus::{
    AecService,
    election::{
        Committee, EpochLedger, EpochValue, ReportIndex, ReportRef, ReportSource, SelectedReport,
    },
};
use crate::utils::diagnostic;
use rsnano_types::{Amount, BlockHash, ConsensusEpoch, PublicKey};
use std::sync::{Arc, Mutex};

/// The checkpoint protocol supplies a decided candidate only after verifying its proof.
/// Phase 0 installs no implementation: account votes cannot decide a checkpoint.
pub struct CheckpointDecision {
    pub value: EpochValue,
    pub proof: Vec<u8>,
}

pub trait CheckpointElection: Send {
    fn candidate(&mut self, value: EpochValue);
    fn decided(&mut self) -> Option<CheckpointDecision>;
    fn verify(&self, decision: &CheckpointDecision) -> bool;
}

pub struct EpochDecisionService {
    exchange: Arc<Mutex<ReportExchange>>,
    active_elections: Arc<AecService>,
    election: Mutex<Option<Box<dyn CheckpointElection>>>,
}
impl EpochDecisionService {
    pub(crate) fn new(
        exchange: Arc<Mutex<ReportExchange>>,
        active_elections: Arc<AecService>,
    ) -> Self {
        Self {
            exchange,
            active_elections,
            election: Mutex::new(None),
        }
    }
    pub fn set_election(&self, election: Box<dyn CheckpointElection>) {
        *self.election.lock().unwrap() = Some(election);
    }
    pub fn tick(&self) {
        let mut election = self.election.lock().unwrap();
        let Some(election) = election.as_mut() else {
            return;
        };
        let epochs = self.exchange.lock().unwrap().pending_epochs();
        for epoch in epochs {
            if self.active_elections.epoch_decided_state(epoch).is_none() && self.can_derive(epoch)
            {
                if let Some((value, _)) = self.derive_fresh(epoch) {
                    election.candidate(value);
                }
            }
        }
        while let Some(decision) = election.decided() {
            if !election.verify(&decision) {
                continue;
            }
            let value = decision.value;
            if let Some((_, state)) = self.validate(&value) {
                self.active_elections
                    .install_decided_checkpoint(value.epoch, state);
            }
        }
    }
    fn can_derive(&self, epoch: ConsensusEpoch) -> bool {
        let Some(committee) = self.active_elections.epoch_committee(epoch) else {
            return false;
        };
        let Some(previous) = self.active_elections.epoch_previous_state(epoch) else {
            return false;
        };
        selected_weight(
            &self.exchange.lock().unwrap(),
            epoch,
            &committee,
            previous.state_hash(),
        ) >= committee.thresholds().report
    }
    fn derive_fresh(&self, epoch: ConsensusEpoch) -> Option<(EpochValue, Arc<EpochLedger>)> {
        let previous = self.active_elections.epoch_previous_state(epoch)?;
        let committee = self.active_elections.epoch_committee(epoch)?;
        let predecessor = previous.state_hash();
        let exchange = self.exchange.lock().unwrap();
        // Only reports signed against the same predecessor checkpoint
        let mut usable: Vec<PublicKey> = exchange
            .usable(epoch)
            .iter()
            .filter(|(report, _, _)| report.predecessor == predecessor)
            .map(|(report, _, _)| report.reporter)
            .collect();
        usable.sort();
        let mut selected: Vec<ReportRef> = Vec::new();
        let mut weight = Amount::ZERO;
        for reporter in usable {
            let Some((report, _, _)) = exchange
                .usable(epoch)
                .into_iter()
                .find(|(report, _, _)| report.reporter == reporter)
            else {
                continue;
            };
            selected.push(ReportRef {
                reporter,
                certified: report.certified,
                residual: report.residual,
            });
            weight = weight
                .number()
                .checked_add(committee.weight(&reporter).number())
                .map(Amount::raw)
                .unwrap_or(Amount::MAX);
            if weight >= committee.thresholds().report {
                break;
            }
        }
        if weight < committee.thresholds().report {
            return None;
        }
        let source = UsableReports {
            exchange: &exchange,
            epoch,
            committee: &committee,
            predecessor,
        };
        let resolved: Vec<(ReportRef, SelectedReport)> = selected
            .iter()
            .filter_map(|report| Some((*report, source.report(report)?)))
            .collect();
        if resolved.len() != selected.len() {
            return None;
        }
        let states: Vec<SelectedReport> = resolved.iter().map(|(_, state)| *state).collect();
        let index = ReportIndex::new(&previous, &states);
        let (value, ledger) = EpochValue::propose(
            epoch,
            &previous,
            &resolved,
            &index,
            committee.thresholds().many,
        );
        Some((value, Arc::new(ledger)))
    }
    fn validate(&self, value: &EpochValue) -> Option<(BlockHash, Arc<EpochLedger>)> {
        let previous = self.active_elections.epoch_previous_state(value.epoch)?;
        let committee = self.active_elections.epoch_committee(value.epoch)?;
        let exchange = self.exchange.lock().unwrap();
        let source = UsableReports {
            exchange: &exchange,
            epoch: value.epoch,
            committee: &committee,
            predecessor: previous.state_hash(),
        };
        let states: Vec<SelectedReport> = value
            .reports()
            .iter()
            .filter_map(|report| source.report(report))
            .collect();
        if states.len() != value.reports().len() {
            return None;
        }
        let index = ReportIndex::new(&previous, &states);
        match value.validate(
            &previous,
            &source,
            &index,
            committee.thresholds().report,
            committee.thresholds().many,
        ) {
            Ok(ledger) => Some((value.hash(), Arc::new(ledger))),
            Err(error) => {
                diagnostic!(
                    "EPOCH_VALUE_REFUSED epoch={} value={} reason={:?}",
                    value.epoch,
                    value.hash(),
                    error
                );
                None
            }
        }
    }
}
struct UsableReports<'a> {
    exchange: &'a ReportExchange,
    epoch: ConsensusEpoch,
    committee: &'a Committee,
    predecessor: BlockHash,
}

impl ReportSource for UsableReports<'_> {
    fn report(&self, report: &ReportRef) -> Option<SelectedReport<'_>> {
        let (certified, residual) = self.exchange.usable_report(
            self.epoch,
            &report.reporter,
            report.certified,
            report.residual,
            self.predecessor,
        )?;
        Some(SelectedReport {
            reporter: report.reporter,
            weight: self.committee.weight(&report.reporter),
            certified,
            residual,
        })
    }
}

/// The weight of the usable reports of an epoch in the committee that issued
/// its votes
fn selected_weight(
    exchange: &ReportExchange,
    epoch: ConsensusEpoch,
    committee: &Committee,
    predecessor: BlockHash,
) -> Amount {
    exchange
        .usable(epoch)
        .iter()
        .filter(|(report, _, _)| report.predecessor == predecessor)
        .fold(Amount::ZERO, |sum, (report, _, _)| {
            sum.number()
                .checked_add(committee.weight(&report.reporter).number())
                .map(Amount::raw)
                .unwrap_or(Amount::MAX)
        })
}
