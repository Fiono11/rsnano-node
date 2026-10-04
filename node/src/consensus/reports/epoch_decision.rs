use super::ReportExchange;
use crate::consensus::{
    AecService,
    election::{
        Committee, EpochLedger, EpochValue, ReportIndex, ReportRef, ReportSource, SelectedReport,
    },
};
use crate::utils::diagnostic;
use rsnano_types::{Amount, BlockHash, ConsensusEpoch, PublicKey};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// The checkpoint protocol supplies a decided candidate only after verifying its proof.
/// Phase 0 installs no implementation: account votes cannot decide a checkpoint.
pub struct CheckpointDecision {
    pub value: EpochValue,
    pub proof: Vec<u8>,
}

pub trait CheckpointElection: Send {
    /// Called after report usability refresh, even without a local candidate.
    /// Implementations should wake pending validation without timer replay.
    fn evidence_updated(&mut self);
    fn candidate(&mut self, value: EpochValue);
    fn decided(&mut self) -> Option<CheckpointDecision>;
    fn verify(&self, decision: &CheckpointDecision) -> bool;
}

pub struct EpochDecisionService {
    exchange: Arc<Mutex<ReportExchange>>,
    active_elections: Arc<AecService>,
    election: Mutex<Option<Box<dyn CheckpointElection>>>,
    candidate_timing: Mutex<CandidateTiming>,
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
            candidate_timing: Mutex::new(CandidateTiming::default()),
        }
    }
    pub fn set_election(&self, election: Box<dyn CheckpointElection>) {
        *self.election.lock().unwrap() = Some(election);
    }
    pub(crate) fn checkpoint_contexts(
        &self,
        session: BlockHash,
    ) -> Vec<crate::consensus::checkpoint_election::CheckpointContext> {
        use crate::consensus::checkpoint_election::{CheckpointContext, CheckpointInstance};
        let epochs = self.exchange.lock().unwrap().pending_epochs();
        epochs
            .into_iter()
            .filter_map(|epoch| {
                let previous = self.active_elections.epoch_previous_state(epoch)?;
                let committee = self.active_elections.epoch_committee(epoch)?;
                CheckpointContext::new(
                    CheckpointInstance {
                        session,
                        epoch,
                        predecessor: previous.state_hash(),
                        committee: committee.digest(),
                    },
                    (*committee).clone(),
                )
                .ok()
            })
            .collect()
    }
    /// Validate against this node's reconstructed signed reports and predecessor,
    /// not metadata asserted by the incoming candidate. Session is supplied by
    /// the configured node/network binding, never copied from the payload.
    pub fn validate_candidate(
        &self,
        session: BlockHash,
        candidate: crate::consensus::checkpoint_election::CheckpointCandidate,
    ) -> Result<
        crate::consensus::checkpoint_election::ValidatedCheckpointCandidate,
        crate::consensus::checkpoint_election::CheckpointError,
    > {
        use crate::consensus::checkpoint_election::{
            CheckpointContext, CheckpointError, CheckpointInstance,
        };
        let epoch = candidate.value.epoch;
        let previous = self.active_elections.epoch_previous_state(epoch).ok_or(
            CheckpointError::MissingEvidence(candidate.instance.predecessor),
        )?;
        let committee = self
            .active_elections
            .epoch_committee(epoch)
            .ok_or(CheckpointError::InvalidCommittee)?;
        let context = CheckpointContext::new(
            CheckpointInstance {
                session,
                epoch,
                predecessor: previous.state_hash(),
                committee: committee.digest(),
            },
            (*committee).clone(),
        )?;
        candidate.authenticate(&context)?;
        let exchange = self.exchange.lock().unwrap();
        let source = UsableReports {
            exchange: &exchange,
            epoch,
            committee: &committee,
            predecessor: previous.state_hash(),
        };
        let states: Vec<SelectedReport> = candidate
            .value
            .reports()
            .iter()
            .filter_map(|reference| source.report(reference))
            .collect();
        if states.len() != candidate.value.reports().len() {
            return Err(CheckpointError::MissingEvidence(candidate.digest()));
        }
        let index = ReportIndex::new(&previous, &states);
        candidate.validate(&context, &previous, &source, &index)
    }

    pub fn tick(&self) {
        self.tick_at(Instant::now());
    }
    fn tick_at(&self, now: Instant) {
        let mut election = self.election.lock().unwrap();
        let Some(election) = election.as_mut() else {
            return;
        };
        election.evidence_updated();
        let epochs = self.exchange.lock().unwrap().pending_epochs();
        for epoch in epochs {
            if self.active_elections.epoch_decided_state(epoch).is_some() {
                self.candidate_timing.lock().unwrap().forget(epoch);
                continue;
            }
            if self.candidate_ready(epoch, now) {
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
                if self
                    .active_elections
                    .epoch_decided_state(value.epoch)
                    .is_some()
                {
                    diagnostic!(
                        "CHECKPOINT_INSTALLED epoch={} value={}",
                        value.epoch,
                        value.hash()
                    );
                }
            }
        }
    }
    fn candidate_ready(&self, epoch: ConsensusEpoch, now: Instant) -> bool {
        let Some(committee) = self.active_elections.epoch_committee(epoch) else {
            return false;
        };
        let Some(previous) = self.active_elections.epoch_previous_state(epoch) else {
            return false;
        };
        let exchange = self.exchange.lock().unwrap();
        let quorum = selected_weight(&exchange, epoch, &committee, previous.state_hash())
            >= committee.thresholds().report;
        let usable = exchange.usable(epoch);
        let digest = committee.digest();
        let all = committee
            .weights()
            .iter()
            .filter(|(_, weight)| !weight.is_zero())
            .all(|(member, _)| {
                usable.iter().any(|(report, _, _)| {
                    report.reporter == *member
                        && report.committee == digest
                        && report.predecessor == previous.state_hash()
                })
            });
        self.candidate_timing.lock().unwrap().ready(
            epoch,
            committee.digest(),
            previous.state_hash(),
            quorum,
            all,
            now,
        )
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
            .filter(|(report, _, _)| {
                report.predecessor == predecessor
                    && report.committee == committee.digest()
                    && !committee.weight(&report.reporter).is_zero()
            })
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
/// Grace begins when a usable report quorum is first observed. Context changes
/// or loss of that quorum discard the old deadline. This clock never gates votes.
#[derive(Default)]
struct CandidateTiming {
    quorum_since: HashMap<ConsensusEpoch, (BlockHash, BlockHash, Instant)>,
}
impl CandidateTiming {
    const GRACE: Duration = Duration::from_millis(500);

    fn ready(
        &mut self,
        epoch: ConsensusEpoch,
        committee: BlockHash,
        predecessor: BlockHash,
        quorum: bool,
        all: bool,
        now: Instant,
    ) -> bool {
        if !quorum {
            self.forget(epoch);
            return false;
        }
        let entry = self
            .quorum_since
            .entry(epoch)
            .or_insert((committee, predecessor, now));
        if entry.0 != committee || entry.1 != predecessor {
            *entry = (committee, predecessor, now);
        }
        all || now.saturating_duration_since(entry.2) >= Self::GRACE
    }

    fn forget(&mut self, epoch: ConsensusEpoch) {
        self.quorum_since.remove(&epoch);
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
        let (_, certified, residual) =
            self.exchange
                .usable(self.epoch)
                .into_iter()
                .find(|(signed, _, _)| {
                    signed.reporter == report.reporter
                        && signed.certified == report.certified
                        && signed.residual == report.residual
                        && signed.predecessor == self.predecessor
                        && signed.committee == self.committee.digest()
                        && !self.committee.weight(&signed.reporter).is_zero()
                })?;
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
        .filter(|(report, _, _)| {
            report.predecessor == predecessor
                && report.committee == committee.digest()
                && !committee.weight(&report.reporter).is_zero()
        })
        .fold(Amount::ZERO, |sum, (report, _, _)| {
            sum.number()
                .checked_add(committee.weight(&report.reporter).number())
                .map(Amount::raw)
                .unwrap_or(Amount::MAX)
        })
}

#[cfg(test)]
mod candidate_context_tests {
    use super::*;
    use crate::consensus::election::{CertifiedState, ResidualVotes};
    use rsnano_types::PrivateKey;
    #[test]
    fn candidate_waits_for_all_reports_or_grace_after_quorum() {
        let mut timing = CandidateTiming::default();
        let epoch = ConsensusEpoch::ZERO;
        let committee = BlockHash::from(1);
        let predecessor = BlockHash::from(2);
        let start = Instant::now();
        assert!(!timing.ready(epoch, committee, predecessor, false, false, start));
        let quorum_at = start + Duration::from_secs(3);
        assert!(!timing.ready(epoch, committee, predecessor, true, false, quorum_at));
        assert!(!timing.ready(
            epoch,
            committee,
            predecessor,
            true,
            false,
            quorum_at + Duration::from_millis(499)
        ));
        assert!(timing.ready(
            epoch,
            committee,
            predecessor,
            true,
            false,
            quorum_at + CandidateTiming::GRACE
        ));
        timing.forget(epoch);
        assert!(timing.ready(epoch, committee, predecessor, true, true, start));
        // Even an inconsistent all-reports flag cannot bypass the quorum.
        assert!(!timing.ready(epoch, committee, predecessor, false, true, start));
    }

    #[test]
    fn candidate_grace_resets_on_context_change_or_quorum_loss() {
        let mut timing = CandidateTiming::default();
        let epoch = ConsensusEpoch::ZERO;
        let start = Instant::now();
        let later = start + Duration::from_secs(1);
        let c = BlockHash::from(1);
        let p = BlockHash::from(2);
        assert!(!timing.ready(epoch, c, p, true, false, start));
        assert!(!timing.ready(epoch, BlockHash::from(3), p, true, false, later));
        assert!(!timing.ready(epoch, c, BlockHash::from(4), true, false, later));
        assert!(!timing.ready(epoch, c, p, false, false, later));
        assert!(!timing.ready(epoch, c, p, true, false, later));
        assert!(timing.ready(epoch, c, p, true, false, later + CandidateTiming::GRACE));
    }

    #[test]
    fn candidate_source_rejects_reports_from_another_committee() {
        let key = PrivateKey::from(1);
        let committee = Committee::equal_weight([key.public_key()], 0, 0).unwrap();
        let mut exchange = ReportExchange::new();
        let certified = CertifiedState::new();
        let residual = ResidualVotes::new();
        let reference = ReportRef {
            reporter: key.public_key(),
            certified: certified.root(),
            residual: residual.root(),
        };
        let predecessor = BlockHash::from(7);
        exchange.report_epoch(
            ConsensusEpoch::ZERO,
            certified,
            residual,
            BlockHash::from(999),
            predecessor,
            &[key],
        );
        let source = UsableReports {
            exchange: &exchange,
            epoch: ConsensusEpoch::ZERO,
            committee: &committee,
            predecessor,
        };
        assert!(source.report(&reference).is_none());
        assert_eq!(
            selected_weight(&exchange, ConsensusEpoch::ZERO, &committee, predecessor),
            Amount::ZERO
        );
    }
}
