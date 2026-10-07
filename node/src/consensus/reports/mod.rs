mod epoch_decision;
mod report_plugin;
mod report_service;
use crate::consensus::election::{
    CertifiedBlock, CertifiedState, ReportCommitment, ResidualKind, ResidualVotes,
};
pub use epoch_decision::EpochDecisionService;
pub(crate) use report_plugin::{ReportPlugin, ReportTicker};
pub use report_service::ReportService;
use rsnano_messages::Report;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey, PublicKey};
use std::collections::{BTreeMap, HashMap};

/// Frozen reports and local reconstructions. Phase 0 only accepts identical roots.
pub(crate) struct ReportExchange {
    epochs: BTreeMap<ConsensusEpoch, EpochReports>,
    max_epochs: usize,
}
#[derive(Default)]
struct EpochReports {
    live: CertifiedState,
    snapshots: BTreeMap<BlockHash, CertifiedState>,
    residuals: HashMap<BlockHash, ResidualVotes>,
    signed: Vec<Report>,
    theirs: HashMap<PublicKey, TheirReport>,
    repeated: Option<Timestamp>,
}
struct TheirReport {
    report: Report,
    reconstructed: Option<CertifiedState>,
    residual: Option<ResidualVotes>,
    derived: Option<Timestamp>,
}
impl TheirReport {
    fn is_complete(&self) -> bool {
        self.reconstructed.is_some() && self.residual.is_some()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReportMessage {
    Broadcast(Report),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReconcileResult {
    pub epoch: ConsensusEpoch,
    pub reporter: PublicKey,
    pub complete: bool,
    pub entries: usize,
    pub total: usize,
}
impl ReportExchange {
    pub const MAX_EPOCHS: usize = 4;
    pub const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);
    pub const REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
    pub fn new() -> Self {
        Self {
            epochs: BTreeMap::new(),
            max_epochs: Self::MAX_EPOCHS,
        }
    }
    pub fn live_root(&self, epoch: ConsensusEpoch) -> Option<BlockHash> {
        self.epochs.get(&epoch).map(|held| held.live.root())
    }
    pub fn report_epoch(
        &mut self,
        epoch: ConsensusEpoch,
        certified: CertifiedState,
        residual: ResidualVotes,
        committee: BlockHash,
        predecessor: BlockHash,
        keys: &[PrivateKey],
    ) -> Vec<ReportMessage> {
        let held = self.epochs.entry(epoch).or_default();
        if !held.signed.is_empty() {
            return Vec::new();
        }
        let certified_root = certified.root();
        let residual_root = residual.root();
        held.signed = keys
            .iter()
            .map(|key| {
                let payload = ReportCommitment {
                    epoch,
                    committee,
                    predecessor,
                    certified: certified_root,
                    residual: residual_root,
                    reporter: key.public_key(),
                }
                .payload();
                Report::new(
                    key,
                    epoch,
                    committee,
                    predecessor,
                    certified_root,
                    residual_root,
                    payload,
                )
            })
            .collect();
        held.snapshots.insert(certified_root, certified.clone());
        for (block, entry) in certified.entries() {
            held.live.certify(*block, entry.previous, entry.status);
        }
        held.residuals.insert(residual_root, residual);
        let messages = held
            .signed
            .iter()
            .cloned()
            .map(ReportMessage::Broadcast)
            .collect();
        self.trim();
        messages
    }
    pub fn has_reported(&self, epoch: ConsensusEpoch) -> bool {
        self.epochs
            .get(&epoch)
            .is_some_and(|held| !held.signed.is_empty())
    }
    pub fn own_reports(&self, epoch: ConsensusEpoch) -> &[Report] {
        self.epochs
            .get(&epoch)
            .map(|held| held.signed.as_slice())
            .unwrap_or_default()
    }
    pub fn repeat_reports(&mut self, now: Timestamp) -> Vec<ReportMessage> {
        let mut messages = Vec::new();
        for held in self.epochs.values_mut() {
            if held.signed.is_empty()
                || held
                    .repeated
                    .is_some_and(|last| last.elapsed(now) < Self::REPEAT_INTERVAL)
            {
                continue;
            }
            held.repeated = Some(now);
            messages.extend(held.signed.iter().cloned().map(ReportMessage::Broadcast));
        }
        messages
    }
    pub fn reports(&self, epoch: ConsensusEpoch) -> Vec<&Report> {
        self.epochs
            .get(&epoch)
            .map(|held| held.theirs.values().map(|their| &their.report).collect())
            .unwrap_or_default()
    }
    pub fn usable_report(
        &self,
        epoch: ConsensusEpoch,
        reporter: &PublicKey,
        certified: BlockHash,
        residual: BlockHash,
        predecessor: BlockHash,
    ) -> Option<(&CertifiedState, &ResidualVotes)> {
        let held = self.epochs.get(&epoch)?;
        // This node's own reports count like any other: a selection is
        // `N - f` reports from distinct old-committee validators, and the
        // reporter is one of them. Leaving its own out would cost a
        // validator its own weight, which the largest member of a committee
        // can not afford.
        if let Some(own) = held
            .signed
            .iter()
            .find(|report| &report.reporter == reporter)
        {
            if own.certified != certified
                || own.residual != residual
                || own.predecessor != predecessor
            {
                return None;
            }
            return Some((held.state(certified)?, held.residuals.get(&residual)?));
        }
        let their = held.theirs.get(reporter)?;
        if their.report.certified != certified
            || their.report.residual != residual
            || their.report.predecessor != predecessor
        {
            return None;
        }
        Some((their.reconstructed.as_ref()?, their.residual.as_ref()?))
    }
    pub fn usable(&self, epoch: ConsensusEpoch) -> Vec<(&Report, &CertifiedState, &ResidualVotes)> {
        let Some(held) = self.epochs.get(&epoch) else {
            return Vec::new();
        };
        let own = held.signed.iter().filter_map(|report| {
            Some((
                report,
                held.state(report.certified)?,
                held.residuals.get(&report.residual)?,
            ))
        });
        let theirs = held.theirs.values().filter_map(|their| {
            Some((
                &their.report,
                their.reconstructed.as_ref()?,
                their.residual.as_ref()?,
            ))
        });
        own.chain(theirs).collect()
    }
    pub fn handle_report(&mut self, report: Report) -> bool {
        let payload = ReportCommitment {
            epoch: report.epoch,
            committee: report.committee,
            predecessor: report.predecessor,
            certified: report.certified,
            residual: report.residual,
            reporter: report.reporter,
        }
        .payload();
        if !report.verify(payload) {
            return false;
        }
        let held = self.epochs.entry(report.epoch).or_default();
        // A reporter signs one report per epoch; a second one changes nothing
        if held.theirs.contains_key(&report.reporter)
            || held
                .signed
                .iter()
                .any(|own| own.reporter == report.reporter)
        {
            return false;
        }
        // An empty residual object is settled by its signed root alone:
        // every replica knows the root of the empty object
        let residual = (report.residual == ResidualVotes::new().root()).then(ResidualVotes::new);
        held.theirs.insert(
            report.reporter,
            TheirReport {
                report,
                reconstructed: None,
                residual,
                derived: None,
            },
        );
        true
    }
    pub fn needs_residual(
        &self,
        epoch: ConsensusEpoch,
        reporter: &PublicKey,
        now: Timestamp,
    ) -> bool {
        self.epochs
            .get(&epoch)
            .and_then(|held| held.theirs.get(reporter))
            .is_some_and(|their| {
                their.reconstructed.is_some()
                    && their.residual.is_none()
                    && their
                        .derived
                        .is_none_or(|last| last.elapsed(now) >= Self::RETRY_INTERVAL)
            })
    }
    pub fn derive_residual(
        &mut self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
        votes: impl IntoIterator<Item = (CertifiedBlock, ResidualKind, BlockHash)>,
        now: Timestamp,
    ) -> Option<ReconcileResult> {
        let held = self.epochs.get_mut(&epoch)?;
        let their = held.theirs.get_mut(&reporter)?;
        let certified = their.reconstructed.as_ref()?;
        if their.residual.is_some() {
            return None;
        }
        their.derived = Some(now);
        let derived = ResidualVotes::derive(certified, votes);
        let total = derived.len();
        let complete = derived.root() == their.report.residual;
        if complete {
            their.residual = Some(derived);
        }
        Some(ReconcileResult {
            epoch,
            reporter,
            complete,
            entries: total,
            total,
        })
    }
    fn trim(&mut self) {
        while self.epochs.len() > self.max_epochs {
            let Some(oldest) = self.epochs.keys().next().copied() else {
                break;
            };
            self.epochs.remove(&oldest);
        }
    }

    pub fn pending_epochs(&self) -> Vec<ConsensusEpoch> {
        self.epochs.keys().copied().collect()
    }
    pub fn refresh_live(&mut self, epoch: ConsensusEpoch, live: CertifiedState) {
        let held = self.epochs.entry(epoch).or_default();
        for (block, entry) in live.entries() {
            held.live.certify(*block, entry.previous, entry.status);
        }
    }
    pub fn reconcile(
        &mut self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
        _now: Timestamp,
    ) -> Option<(Vec<ReportMessage>, Option<ReconcileResult>)> {
        let held = self.epochs.get_mut(&epoch)?;
        let their = held.theirs.get(&reporter)?;
        if their.is_complete() {
            return None;
        }
        let state = held.state(their.report.certified).cloned();
        let their = held.theirs.get_mut(&reporter)?;
        if let Some(state) = state {
            let total = state.len();
            their.reconstructed = Some(state);
            return Some((
                Vec::new(),
                Some(ReconcileResult {
                    epoch,
                    reporter,
                    complete: their.is_complete(),
                    entries: 0,
                    total,
                }),
            ));
        }
        Some((Vec::new(), None))
    }
}
impl EpochReports {
    fn state(&self, root: BlockHash) -> Option<&CertifiedState> {
        if self.live.root() == root {
            return Some(&self.live);
        }
        if let Some(state) = self.snapshots.get(&root) {
            return Some(state);
        }
        self.theirs
            .values()
            .filter_map(|their| their.reconstructed.as_ref())
            .find(|state| state.root() == root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::election::CertifiedStatus;
    use rsnano_types::Account;

    fn block() -> CertifiedBlock {
        CertifiedBlock::new(Account::from(1), 1, BlockHash::from(2))
    }

    fn report(state: CertifiedState, residual: ResidualVotes) -> Report {
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            ConsensusEpoch::ZERO,
            state,
            residual,
            BlockHash::from(3),
            BlockHash::from(4),
            &[PrivateKey::from(1)],
        );
        reporter.own_reports(ConsensusEpoch::ZERO)[0].clone()
    }

    #[test]
    fn identical_root_is_usable_without_messages() {
        let mut state = CertifiedState::new();
        state.certify(block(), BlockHash::ZERO, CertifiedStatus::Notarized);
        let report = report(state.clone(), ResidualVotes::new());
        let mut exchange = ReportExchange::new();
        exchange.refresh_live(report.epoch, state);
        assert!(exchange.handle_report(report.clone()));
        let (messages, result) = exchange
            .reconcile(
                report.epoch,
                report.reporter,
                Timestamp::new_test_instance(),
            )
            .unwrap();
        assert!(messages.is_empty());
        assert!(result.unwrap().complete);
        assert_eq!(exchange.usable(report.epoch).len(), 1);
    }

    #[test]
    fn different_root_stays_unusable_until_local_evidence_matches() {
        let mut state = CertifiedState::new();
        state.certify(block(), BlockHash::ZERO, CertifiedStatus::Finalized);
        let report = report(state.clone(), ResidualVotes::new());
        let mut exchange = ReportExchange::new();
        exchange.handle_report(report.clone());
        let (messages, result) = exchange
            .reconcile(
                report.epoch,
                report.reporter,
                Timestamp::new_test_instance(),
            )
            .unwrap();
        assert!(messages.is_empty());
        assert!(result.is_none());
        assert!(exchange.usable(report.epoch).is_empty());
        exchange.refresh_live(report.epoch, state);
        exchange.reconcile(
            report.epoch,
            report.reporter,
            Timestamp::new_test_instance(),
        );
        assert_eq!(exchange.usable(report.epoch).len(), 1);
    }

    #[test]
    fn both_roots_must_match() {
        let mut votes = ResidualVotes::new();
        votes.record(block(), BlockHash::ZERO, ResidualKind::First);
        let report = report(CertifiedState::new(), votes);
        let mut exchange = ReportExchange::new();
        exchange.handle_report(report.clone());
        let now = Timestamp::new_test_instance();
        exchange.reconcile(report.epoch, report.reporter, now);
        assert!(exchange.usable(report.epoch).is_empty());
        exchange.derive_residual(report.epoch, report.reporter, [], now);
        assert!(exchange.usable(report.epoch).is_empty());
        exchange.derive_residual(
            report.epoch,
            report.reporter,
            [(block(), ResidualKind::First, BlockHash::ZERO)],
            now,
        );
        assert_eq!(exchange.usable(report.epoch).len(), 1);
    }

    #[test]
    fn report_signature_and_identity_are_checked() {
        let report = report(CertifiedState::new(), ResidualVotes::new());
        let mut exchange = ReportExchange::new();
        let mut altered = report.clone();
        altered.certified = BlockHash::from(99);
        assert!(!exchange.handle_report(altered));
        assert!(exchange.handle_report(report.clone()));
        assert!(!exchange.handle_report(report));
    }

    #[test]
    fn own_snapshot_is_frozen_and_not_counted_twice() {
        let mut exchange = ReportExchange::new();
        let epoch = ConsensusEpoch::ZERO;
        exchange.report_epoch(
            epoch,
            CertifiedState::new(),
            ResidualVotes::new(),
            BlockHash::from(3),
            BlockHash::from(4),
            &[PrivateKey::from(1)],
        );
        let own = exchange.own_reports(epoch)[0].clone();
        assert!(!exchange.handle_report(own.clone()));
        let mut live = CertifiedState::new();
        live.certify(block(), BlockHash::ZERO, CertifiedStatus::Finalized);
        exchange.refresh_live(epoch, live);
        assert_ne!(exchange.live_root(epoch), Some(own.certified));
        assert_eq!(exchange.usable(epoch).len(), 1);
        assert_eq!(exchange.usable(epoch)[0].1.root(), own.certified);
    }
}
