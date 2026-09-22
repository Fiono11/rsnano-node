mod report_plugin;
mod report_service;

pub(crate) use report_plugin::{ReportPlugin, ReportTicker};
pub use report_service::ReportService;

use std::collections::{BTreeMap, HashMap};

use rsnano_messages::{CertifiedEntry, ReconReply, ReconReq, Report};
use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey, PublicKey};

use crate::consensus::election::{
    CertifiedBlock, CertifiedState, CertifiedStatus, ReportCommitment, ResidualVotes,
};

/// RAI, "Certified-state reports and reconciliation": the reports of one run.
///
/// For each epoch this node keeps its live certified state, which goes on
/// growing as gossip delivers votes, and the snapshot it froze when it signed
/// its report. Both are states it knows, so either can be the source or the
/// target of a reconstructive difference; that is what lets a validator which
/// has converged to a later common state reach a historical root.
///
/// The reports of the other validators are kept as their signed roots until
/// something has to be validated against them, and reconstructed then.
///
/// Pure state: what to send is returned to the caller, which owns the
/// network. Nothing here reads a clock or a socket.
pub(crate) struct ReportExchange {
    epochs: BTreeMap<ConsensusEpoch, EpochReports>,
    /// Epochs kept; older ones are dropped with their states
    max_epochs: usize,
}

#[derive(Default)]
struct EpochReports {
    /// The certified state as it stands here, which keeps growing
    live: CertifiedState,
    /// The states this node retains by their roots: the one its report
    /// froze, and any other worth keeping as a bridge
    history: BTreeMap<BlockHash, CertifiedState>,
    /// The residual votes this node signed for
    residual: ResidualVotes,
    /// One signed report per representative this node votes with
    signed: Vec<Report>,
    /// The reports of the other validators, by reporter
    theirs: HashMap<PublicKey, TheirReport>,
}

struct TheirReport {
    report: Report,
    /// The state reconstructed for the signed root, once a difference has
    /// rebuilt it
    reconstructed: Option<CertifiedState>,
}

impl TheirReport {
    fn is_complete(&self) -> bool {
        self.reconstructed.is_some()
    }
}

/// What the exchange asks the caller to send
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReportMessage {
    /// Broadcast this node's own report
    Broadcast(Report),
    /// Ask for a reconstructive difference towards a report's root
    Request(ReconReq),
    /// Answer a request with the difference between two states this node knows
    Reply(ReconReply),
}

/// What a reconciliation cost and whether it succeeded, for the record
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReconcileResult {
    pub epoch: ConsensusEpoch,
    pub reporter: PublicKey,
    /// The rebuilt state hashes to the root the reporter signed
    pub complete: bool,
    /// Entries the difference carried
    pub entries: usize,
    /// Entries in the rebuilt state
    pub total: usize,
}

#[allow(dead_code)] // the readers of a usable report are the epoch decision's
impl ReportExchange {
    /// Epochs whose states are kept: the closing one and a little history,
    /// so a validator that lags can still reconstruct
    pub const MAX_EPOCHS: usize = 4;

    pub fn new() -> Self {
        Self {
            epochs: BTreeMap::new(),
            max_epochs: Self::MAX_EPOCHS,
        }
    }

    /// The root of the epoch's certified state as it stands here
    pub fn live_root(&self, epoch: ConsensusEpoch) -> Option<BlockHash> {
        self.epochs.get(&epoch).map(|held| held.live.root())
    }

    /// RAI: this node stops issuing account votes for the epoch and signs one
    /// report per representative it votes with. The certified state is frozen
    /// as the report's snapshot; the live state carries on from there.
    pub fn report_epoch(
        &mut self,
        epoch: ConsensusEpoch,
        certified: CertifiedState,
        residual: ResidualVotes,
        committee: BlockHash,
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
                    certified: certified_root,
                    residual: residual_root,
                    reporter: key.public_key(),
                }
                .payload();
                Report::new(
                    key,
                    epoch,
                    committee,
                    certified_root,
                    residual_root,
                    payload,
                )
            })
            .collect();
        held.history.insert(certified_root, certified.clone());
        held.live = certified;
        held.residual = residual;
        let messages = held
            .signed
            .iter()
            .cloned()
            .map(ReportMessage::Broadcast)
            .collect();
        self.trim();
        messages
    }

    /// Whether this node has reported on the epoch
    pub fn has_reported(&self, epoch: ConsensusEpoch) -> bool {
        self.epochs
            .get(&epoch)
            .is_some_and(|held| !held.signed.is_empty())
    }

    /// The signed reports of this node for the epoch, one per representative
    pub fn own_reports(&self, epoch: ConsensusEpoch) -> &[Report] {
        self.epochs
            .get(&epoch)
            .map(|held| held.signed.as_slice())
            .unwrap_or_default()
    }

    /// RAI: the certified state of an epoch grows here as gossip delivers the
    /// votes behind a certificate. The state a report signed stays in the
    /// history, so this node can still bridge to it, and so does the state
    /// it was asked to bridge from: a requester which advertised a root has
    /// to be answerable once this node reaches it.
    pub fn refresh_live(&mut self, epoch: ConsensusEpoch, live: CertifiedState) {
        let held = self.epochs.entry(epoch).or_default();
        if held.live.root() == live.root() {
            return;
        }
        // Keep the state left behind: it is a common descendant for anyone
        // who advertised it, and the bridge to every root before it
        let previous = std::mem::replace(&mut held.live, live);
        if !previous.is_empty() {
            held.history.insert(previous.root(), previous);
        }
        held.trim_history();
    }

    /// The reports of the epoch this node holds, reconstructed or not
    pub fn reports(&self, epoch: ConsensusEpoch) -> Vec<&Report> {
        self.epochs
            .get(&epoch)
            .map(|held| held.theirs.values().map(|their| &their.report).collect())
            .unwrap_or_default()
    }

    /// The reports whose certified state has been reconstructed and checked
    /// against the signed root: what an epoch proposal may select
    pub fn usable(&self, epoch: ConsensusEpoch) -> Vec<(&Report, &CertifiedState)> {
        let Some(held) = self.epochs.get(&epoch) else {
            return Vec::new();
        };
        held.theirs
            .values()
            .filter_map(|their| {
                their
                    .reconstructed
                    .as_ref()
                    .map(|state| (&their.report, state))
            })
            .collect()
    }

    /// Lemma 3.5: a report is taken only with a valid signature over its
    /// epoch, committee, both roots and the reporter. It is stored, not
    /// reconstructed: the contents are fetched when something has to be
    /// validated against them.
    pub fn handle_report(&mut self, report: Report) -> bool {
        let payload = ReportCommitment {
            epoch: report.epoch,
            committee: report.committee,
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
        if held.theirs.contains_key(&report.reporter) {
            return false;
        }
        held.theirs.insert(
            report.reporter,
            TheirReport {
                report,
                reconstructed: None,
            },
        );
        true
    }

    /// RAI: ask for the difference from the state this node holds to a
    /// report's root. A state that already hashes to the root needs no
    /// difference at all, which is the common case between validators that
    /// saw the same evidence.
    pub fn reconcile(
        &mut self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
    ) -> Option<(Option<ReportMessage>, Option<ReconcileResult>)> {
        let held = self.epochs.get_mut(&epoch)?;
        let source = held.live.root();
        let live = held.live.clone();
        let their = held.theirs.get_mut(&reporter)?;
        if their.is_complete() {
            return None;
        }
        let target = their.report.certified;
        if source == target {
            let total = live.len();
            their.reconstructed = Some(live);
            return Some((
                None,
                Some(ReconcileResult {
                    epoch,
                    reporter,
                    complete: true,
                    entries: 0,
                    total,
                }),
            ));
        }
        Some((
            Some(ReportMessage::Request(ReconReq {
                epoch,
                source,
                target,
            })),
            None,
        ))
    }

    /// RAI: answer a request, if this node knows both states. The source may
    /// be its live state or one it retained, and so may the target; a replica
    /// that knows only one of them does not answer.
    pub fn handle_request(&self, request: &ReconReq) -> Option<ReconReply> {
        let held = self.epochs.get(&request.epoch)?;
        let source = held.state(request.source)?;
        let target = held.state(request.target)?;
        let delta = source.difference(target)?;
        if delta.entries.len() > ReconReply::MAX_ENTRIES {
            return None;
        }
        Some(ReconReply {
            epoch: request.epoch,
            source: request.source,
            target: request.target,
            entries: delta
                .entries
                .iter()
                .map(|(block, status)| CertifiedEntry {
                    account: block.account,
                    height: block.height,
                    hash: block.hash,
                    status: match status {
                        CertifiedStatus::Notarized => 0,
                        CertifiedStatus::Finalized => 1,
                        CertifiedStatus::FastFinalized => 2,
                    },
                })
                .collect(),
        })
    }

    /// RAI: apply a difference and accept the reconstruction exactly when the
    /// root comes out as the one the report signed. That check is the whole
    /// of the reconciliation: the reply carries no signature of its own.
    pub fn handle_reply(&mut self, reply: &ReconReply) -> Option<ReconcileResult> {
        let held = self.epochs.get_mut(&reply.epoch)?;
        let mut state = held.state(reply.source)?.clone();
        let reporter = held
            .theirs
            .iter()
            .find(|(_, their)| their.report.certified == reply.target && !their.is_complete())
            .map(|(reporter, _)| *reporter)?;
        for entry in &reply.entries {
            state.certify(
                CertifiedBlock::new(entry.account, entry.height, entry.hash),
                match entry.status {
                    1 => CertifiedStatus::Finalized,
                    2 => CertifiedStatus::FastFinalized,
                    _ => CertifiedStatus::Notarized,
                },
            );
        }
        let complete = state.root() == reply.target;
        let total = state.len();
        if complete {
            held.theirs.get_mut(&reporter)?.reconstructed = Some(state);
        }
        Some(ReconcileResult {
            epoch: reply.epoch,
            reporter,
            complete,
            entries: reply.entries.len(),
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
}

impl EpochReports {
    /// The states an epoch retains besides the live one: the report's own
    /// snapshot and the states this node passed through, which are what it
    /// can bridge from
    const MAX_HISTORY: usize = 8;

    /// A state this node knows by its root: the live one, or one it retained
    fn state(&self, root: BlockHash) -> Option<&CertifiedState> {
        if self.live.root() == root {
            return Some(&self.live);
        }
        self.history.get(&root)
    }

    /// Drops the smallest retained states, never the signed ones: a report's
    /// snapshot is what the other validators ask this node to bridge to
    fn trim_history(&mut self) {
        let signed: Vec<BlockHash> = self.signed.iter().map(|report| report.certified).collect();
        while self.history.len() > Self::MAX_HISTORY {
            let Some(drop) = self
                .history
                .iter()
                .filter(|(root, _)| !signed.contains(root))
                .min_by_key(|(_, state)| state.len())
                .map(|(root, _)| *root)
            else {
                break;
            };
            self.history.remove(&drop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::Account;

    #[test]
    fn reporting_an_epoch_signs_one_report_per_representative() {
        let mut exchange = ReportExchange::new();
        let keys = [PrivateKey::from(1), PrivateKey::from(2)];
        let certified = state_of(0..5);
        let root = certified.root();

        let messages = exchange.report_epoch(
            ConsensusEpoch::ZERO,
            certified,
            ResidualVotes::new(),
            BlockHash::from(7),
            &keys,
        );

        assert_eq!(messages.len(), 2);
        assert!(exchange.has_reported(ConsensusEpoch::ZERO));
        assert_eq!(exchange.live_root(ConsensusEpoch::ZERO), Some(root));
        for (message, key) in messages.iter().zip(keys.iter()) {
            let ReportMessage::Broadcast(report) = message else {
                panic!("expected a broadcast");
            };
            assert_eq!(report.reporter, key.public_key());
            assert_eq!(report.certified, root);
        }
        // An epoch is reported once
        assert!(
            exchange
                .report_epoch(
                    ConsensusEpoch::ZERO,
                    state_of(0..9),
                    ResidualVotes::new(),
                    BlockHash::from(7),
                    &keys,
                )
                .is_empty()
        );
        assert_eq!(exchange.live_root(ConsensusEpoch::ZERO), Some(root));
    }

    /// Lemma 3.5: the reporter's signature binds both roots
    #[test]
    fn a_report_with_a_bad_signature_is_ignored() {
        let mut exchange = ReportExchange::new();
        let mut report = signed(&PrivateKey::from(1), ConsensusEpoch::ZERO, &state_of(0..3));
        report.certified = BlockHash::from(999);
        assert!(!exchange.handle_report(report));
        assert!(exchange.reports(ConsensusEpoch::ZERO).is_empty());
    }

    /// A validator whose own state already hashes to the report's root
    /// reconstructs it without asking for anything
    #[test]
    fn an_equal_state_needs_no_difference() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let theirs = state_of(0..10);
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            theirs.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &theirs)));

        let (message, result) = ours.reconcile(epoch, key.public_key()).unwrap();
        assert!(message.is_none(), "nothing to ask for");
        let result = result.expect("complete at once");
        assert!(result.complete);
        assert_eq!(result.entries, 0);
        assert_eq!(ours.usable(epoch).len(), 1);
        // And it is not reconciled twice
        assert!(ours.reconcile(epoch, key.public_key()).is_none());
    }

    /// The reconciliation: a validator behind the reporter asks for the
    /// difference, a validator that knows both states bridges, and the root
    /// comes out as the signed one
    #[test]
    fn a_report_is_reconstructed_from_a_difference() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let theirs = state_of(0..40);
        // This node saw 35 of the 40 certificates
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            state_of(0..35),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &theirs)));

        let (message, result) = ours.reconcile(epoch, key.public_key()).unwrap();
        assert!(result.is_none());
        let Some(ReportMessage::Request(request)) = message else {
            panic!("expected a request");
        };

        // A validator that reported the same 35 and has since seen the other
        // five knows both the source and the target
        let mut bridging = ReportExchange::new();
        bridging.report_epoch(
            epoch,
            state_of(0..35),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(3)],
        );
        bridging.refresh_live(epoch, state_of(0..40));
        let reply = bridging.handle_request(&request).expect("it knows both");
        assert_eq!(reply.entries.len(), 5);

        let result = ours.handle_reply(&reply).expect("a reply we asked for");
        assert!(result.complete);
        assert_eq!(result.entries, 5);
        assert_eq!(result.total, 40);
        let usable = ours.usable(epoch);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].1.root(), theirs.root());
    }

    /// Lemma 3.6: a validator whose live state has moved on can still bridge
    /// from the state it was at, because it retains the states it passed
    /// through. That is what makes a request answerable after the fact.
    #[test]
    fn a_state_left_behind_still_bridges() {
        let epoch = ConsensusEpoch::ZERO;
        let mut exchange = ReportExchange::new();
        exchange.report_epoch(
            epoch,
            state_of(0..10),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(1)],
        );
        let reported = exchange.live_root(epoch).unwrap();
        exchange.refresh_live(epoch, state_of(0..20));
        let passed = exchange.live_root(epoch).unwrap();
        exchange.refresh_live(epoch, state_of(0..30));

        // The report's own root, and the state passed through on the way,
        // are both still bridgeable to the live one
        for source in [reported, passed] {
            let reply = exchange
                .handle_request(&ReconReq {
                    epoch,
                    source,
                    target: exchange.live_root(epoch).unwrap(),
                })
                .expect("both states are known");
            assert!(!reply.entries.is_empty());
        }
        // And the live state bridges back to the report, which is what a
        // validator asking for the historical root needs
        let reply = exchange
            .handle_request(&ReconReq {
                epoch,
                source: reported,
                target: reported,
            })
            .unwrap();
        assert!(reply.entries.is_empty());
    }

    /// The signed snapshot is never dropped to make room: it is the root the
    /// other validators ask this node to bridge to
    #[test]
    fn the_reported_state_survives_a_long_history() {
        let epoch = ConsensusEpoch::ZERO;
        let mut exchange = ReportExchange::new();
        exchange.report_epoch(
            epoch,
            state_of(0..5),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(1)],
        );
        let reported = exchange.live_root(epoch).unwrap();
        for i in 6..40 {
            exchange.refresh_live(epoch, state_of(0..i));
        }
        let reply = exchange.handle_request(&ReconReq {
            epoch,
            source: reported,
            target: exchange.live_root(epoch).unwrap(),
        });
        assert!(reply.is_some(), "the reported state is still known");
    }

    /// A replica that does not know both states returns nothing, which the
    /// requester treats as no answer rather than as a verdict
    #[test]
    fn a_replica_that_knows_one_state_does_not_answer() {
        let epoch = ConsensusEpoch::ZERO;
        let mut exchange = ReportExchange::new();
        exchange.report_epoch(
            epoch,
            state_of(0..10),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(1)],
        );
        let known = exchange.live_root(epoch).unwrap();
        for (source, target) in [
            (known, BlockHash::from(12345)),
            (BlockHash::from(12345), known),
        ] {
            assert!(
                exchange
                    .handle_request(&ReconReq {
                        epoch,
                        source,
                        target
                    })
                    .is_none()
            );
        }
        // Both known: the difference is empty
        let reply = exchange
            .handle_request(&ReconReq {
                epoch,
                source: known,
                target: known,
            })
            .unwrap();
        assert!(reply.entries.is_empty());
    }

    /// A reply that does not rebuild the signed root leaves the report unusable
    #[test]
    fn a_reply_that_misses_the_root_is_not_usable() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            state_of(0..8),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &state_of(0..10))));
        let (message, _) = ours.reconcile(epoch, key.public_key()).unwrap();
        let Some(ReportMessage::Request(request)) = message else {
            panic!("expected a request");
        };

        // One entry short of the target
        let reply = ReconReply {
            epoch,
            source: request.source,
            target: request.target,
            entries: vec![CertifiedEntry {
                account: block(8).account,
                height: block(8).height,
                hash: block(8).hash,
                status: 0,
            }],
        };
        let result = ours.handle_reply(&reply).unwrap();
        assert!(!result.complete);
        assert!(ours.usable(epoch).is_empty());
    }

    /// Only the recent epochs are kept
    #[test]
    fn old_epochs_are_dropped() {
        let mut exchange = ReportExchange::new();
        let key = PrivateKey::from(1);
        for i in 0..(ReportExchange::MAX_EPOCHS as u64 + 2) {
            exchange.report_epoch(
                ConsensusEpoch::new(i),
                state_of(0..2),
                ResidualVotes::new(),
                BlockHash::from(7),
                &[key.clone()],
            );
        }
        assert!(exchange.live_root(ConsensusEpoch::ZERO).is_none());
        assert!(exchange.live_root(ConsensusEpoch::new(1)).is_none());
        assert!(exchange.live_root(ConsensusEpoch::new(2)).is_some());
    }

    /*
     * Test helpers
     */

    fn block(i: u64) -> CertifiedBlock {
        CertifiedBlock::new(Account::from(i), 1 + i % 4, BlockHash::from(i * 7 + 1))
    }

    fn state_of(blocks: std::ops::Range<u64>) -> CertifiedState {
        let mut state = CertifiedState::new();
        for i in blocks {
            state.certify(block(i), CertifiedStatus::Notarized);
        }
        state
    }

    fn signed(key: &PrivateKey, epoch: ConsensusEpoch, state: &CertifiedState) -> Report {
        let residual = ResidualVotes::new();
        let payload = ReportCommitment {
            epoch,
            committee: BlockHash::from(7),
            certified: state.root(),
            residual: residual.root(),
            reporter: key.public_key(),
        }
        .payload();
        Report::new(
            key,
            epoch,
            BlockHash::from(7),
            state.root(),
            residual.root(),
            payload,
        )
    }
}
