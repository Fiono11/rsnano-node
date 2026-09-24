mod checkpoint;
mod close_proof;
mod epoch_decision;
mod ledger_evidence;
mod report_plugin;
mod report_service;
mod residual_data;

pub use epoch_decision::EpochDecisionService;
pub(crate) use report_plugin::{ReportPlugin, ReportTicker};
pub use report_service::ReportService;

use std::collections::{BTreeMap, HashMap};

use rsnano_messages::{
    CertifiedEntry, LedgerSketchReply, LedgerSketchReq, ReconReply, ReconReq, Report,
    SketchCellWire,
};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey, PublicKey};

use crate::consensus::election::{
    Certification, CertifiedBlock, CertifiedState, CertifiedStatus, ReportCommitment, ResidualKind,
    ResidualVotes, Sketch, SketchCell,
};
pub(crate) use ledger_evidence::CertificateSource;
use ledger_evidence::unjustified_entries;

/// RAI, "Reports that remain reconstructible": the reports of one run.
///
/// For each epoch this node keeps its live certified state, which goes on
/// growing as gossip delivers votes, and the snapshot it froze when it signed
/// its report. Both are states it knows, so either can be the source or the
/// target of a reconstructive difference; that is what lets a validator which
/// has converged to a later common state reach a historical root.
///
/// The reports of the other validators are kept as their signed roots until
/// something has to be validated against them, and reconstructed then: a
/// requester names the roots it holds and the root it wants, and a replica
/// that knows both answers with the canonical edits between the two. There
/// is no other route to a state. "Correct validators continuously update
/// their canonical local inventories and roots as historical records
/// arrive", so the live roots converge, and a retry then finds a source the
/// requester and a correct reporter share.
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
    predecessor: Option<(
        BlockHash,
        std::sync::Arc<crate::consensus::election::EpochLedger>,
    )>,
    /// H(K_e): the digest of the committee that issued the epoch's votes,
    /// which every report of the epoch must bind
    committee: Option<BlockHash>,
    /// The certified state as it stands here. It only ever grows: "correct
    /// validators continuously update their canonical local inventories and
    /// roots as historical records arrive", and a certificate once
    /// constructed stays a historical record whatever became of the
    /// instance it was constructed in. That is what makes the live roots of
    /// two correct validators converge.
    live: CertifiedState,
    /// The states this node retains by their roots: the one its report
    /// froze, and the ones it passed through, which are what it can bridge
    /// from
    history: BTreeMap<BlockHash, CertifiedState>,
    /// The residual object this node's reports committed to, by its root
    residuals: HashMap<BlockHash, ResidualVotes>,
    /// One signed report per representative this node votes with
    signed: Vec<Report>,
    /// The reports of the other validators, by reporter
    theirs: HashMap<PublicKey, TheirReport>,
    /// When this node last repeated its own reports
    repeated: Option<Timestamp>,
}

struct TheirReport {
    report: Report,
    /// When this node last asked for a part of the report it lacks
    asked: Option<Timestamp>,
    /// When it first asked: a sketch is added once a root-based request has
    /// stood unanswered for a while
    first_asked: Option<Timestamp>,
    /// The state reconstructed for the signed root, once a difference has
    /// rebuilt it
    reconstructed: Option<CertifiedState>,
    /// RAI, "Reconstructing a report": whether every N and F membership of
    /// the reconstructed state is justified by a certificate assembled here
    /// from retained signed votes, or by the verified predecessor. Hash
    /// equality authenticates the tag; it does not prove the quorum.
    evidence: EvidenceState,
    transfer: Option<ReconTransfer>,
    /// RAI: the residual object `g_i` commits to, once derived from the
    /// reporter's votes held here and checked against the signed root. A
    /// report is usable only with both.
    residual: Option<ResidualVotes>,
    /// When the derivation was last tried
    derived: Option<Timestamp>,
    /// The object as derived here when it did not hash to the signed root,
    /// for the diagnostics: what is missing is read off it. Never a source
    /// of usability.
    working: Option<ResidualVotes>,
    /// RAI, "Reconstructing a report": the sketch exchange towards the
    /// frozen `T_i` when no root this node holds is known to any responder.
    /// The snapshot the sketch described, and the pages received for it.
    sketch: Option<LedgerSketch>,
    /// When a sketch was last sent
    sketched: Option<Timestamp>,
    /// Cells in the sketch sent for it; grows while the difference does not
    /// peel out
    cells: usize,
}

/// RAI: how far the N/F memberships of a reconstructed report are justified
#[derive(Clone, Debug, PartialEq, Eq)]
enum EvidenceState {
    /// Not checked yet, or the predecessor was not known when it was tried
    Unchecked,
    /// Checked; these entries lack their certificate here. Their signed
    /// votes are asked for and the check is repeated.
    Missing {
        hashes: Vec<BlockHash>,
        checked: Timestamp,
    },
    /// Every N and F entry has its certificate or inherited justification
    Verified,
}

/// One bounded transfer per report. Partial pages are never usable evidence.
struct ReconTransfer {
    source: BlockHash,
    base: CertifiedState,
    pages: Vec<Option<ReconReply>>,
}

/// The sketch exchange of one report: the snapshot sketched and the pages
/// of the difference peeled out of it. Partial pages are never usable.
struct LedgerSketch {
    base: CertifiedState,
    pages: Vec<Option<LedgerSketchReply>>,
}

impl TheirReport {
    /// RAI: a report is usable only after reconstructing `T_i` and `G_i` and
    /// recomputing both signed roots. A certified state without the
    /// reporter's residual votes would lose every candidate that has no
    /// certificate, which is what `Include_Q` and `A_Q` rest on.
    fn is_complete(&self) -> bool {
        self.reconstructed.is_some() && self.residual.is_some()
    }

    /// Usable: both roots reconstructed and every membership justified
    fn is_usable(&self) -> bool {
        self.is_complete() && self.evidence == EvidenceState::Verified
    }
}

/// What the exchange asks the caller to send
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReportMessage {
    /// Broadcast this node's own report
    Broadcast(Report),
    /// Ask for the difference from a state this node holds to a report's root
    Request(ReconReq),
    /// Answer a request with the difference between two states this node knows
    Reply(ReconReply),
    /// Ask for the difference from a sketched snapshot to a report's root
    Sketch(LedgerSketchReq),
    /// Answer a sketch with the difference peeled out of it
    SketchReply(LedgerSketchReply),
}

/// Why a request for a difference is not answered: no answer is not a
/// verdict, but the reason is worth a diagnostic
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReconRefusal {
    /// The target root is no state this node knows
    UnknownTarget,
    /// None of the sources offered is a state this node knows
    UnknownSource,
    /// The difference from the closest known source has this many edits,
    /// more than a reply carries
    TooLarge(usize),
}

/// What a reconciliation cost and whether it succeeded, for the record
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReconcileResult {
    pub epoch: ConsensusEpoch,
    pub reporter: PublicKey,
    /// Both halves of the report hash to the roots the reporter signed
    pub complete: bool,
    /// Entries the answer carried
    pub entries: usize,
    /// Entries in the rebuilt object
    pub total: usize,
}

#[allow(dead_code)] // the readers of a usable report are the epoch decision's
impl ReportExchange {
    /// Epochs whose states are kept: the closing one and a little history,
    /// so a validator that lags can still reconstruct
    pub const MAX_EPOCHS: usize = 4;
    /// How long an unanswered request stands before it is repeated. A
    /// request is gossiped, and the states it names change no faster than
    /// the epoch's evidence arrives.
    pub const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);
    /// How often this node repeats its signed reports. The broadcast at the
    /// boundary is one message under the heaviest traffic of the epoch; a
    /// validator that missed it can not select this node's report, and
    /// "correct validators retain and gossip the votes and data that
    /// justify their actions".
    pub const REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
    /// How often a sketch is sent for a report whose root-based request went
    /// unanswered: a sketch is larger than a request, and the difference it
    /// asks for changes no faster than the evidence arrives
    pub const SKETCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

    pub fn new() -> Self {
        Self {
            epochs: BTreeMap::new(),
            max_epochs: Self::MAX_EPOCHS,
        }
    }

    /// The root of the epoch's certified state as it stands here
    pub fn set_predecessor(
        &mut self,
        epoch: ConsensusEpoch,
        state: std::sync::Arc<crate::consensus::election::EpochLedger>,
    ) {
        let held = self.epochs.entry(epoch).or_default();
        if held
            .predecessor
            .as_ref()
            .is_some_and(|(_, old)| std::sync::Arc::ptr_eq(old, &state))
        {
            return;
        }
        held.predecessor = Some((state.state_hash(), state));
    }

    /// The committee the epoch's reports must bind, once known here
    pub fn set_committee(&mut self, epoch: ConsensusEpoch, digest: BlockHash) {
        self.epochs.entry(epoch).or_default().committee = Some(digest);
    }

    pub fn live_root(&self, epoch: ConsensusEpoch) -> Option<BlockHash> {
        self.epochs.get(&epoch).map(|held| held.live.root())
    }

    /// RAI: this node stopped issuing account votes for the epoch at its
    /// boundary and signs one report per representative it votes with,
    /// against the closed predecessor checkpoint. The certified state is
    /// frozen as the report's snapshot; the live state carries on from
    /// there.
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
        held.history.insert(certified_root, certified.clone());
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

    /// Install the complete canonical epoch projection, including removals
    /// caused by selected-prefix finality. Signed snapshots remain in history.
    pub fn refresh_live(&mut self, epoch: ConsensusEpoch, live: CertifiedState) {
        let held = self.epochs.entry(epoch).or_default();
        if held.live.root() == live.root() {
            return;
        }
        let before = std::mem::replace(&mut held.live, live);
        // Keep the state left behind: it is a common descendant for anyone
        // who advertised it, and the bridge to every root before it
        if !before.is_empty() {
            held.history.insert(before.root(), before);
        }
        held.trim_history();
    }

    /// RAI: this node's signed reports of every epoch it still holds, to
    /// broadcast again, once per `REPEAT_INTERVAL`
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

    /// The reports of the epoch this node holds, reconstructed or not
    pub fn reports(&self, epoch: ConsensusEpoch) -> Vec<&Report> {
        self.epochs
            .get(&epoch)
            .map(|held| held.theirs.values().map(|their| &their.report).collect())
            .unwrap_or_default()
    }

    /// RAI: the reconstructed contents of one report an epoch proposal
    /// names, if this node holds it and both roots match what the reporter
    /// signed. A proposal naming a report it has not reconstructed is one it
    /// can not check, and it does not vote for it.
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
            let state = held.state(certified)?;
            let votes = held.residuals.get(&residual)?;
            if !held.valid_membership(own, state, votes) {
                return None;
            }
            return Some((state, votes));
        }
        let their = held.theirs.get(reporter)?;
        if their.report.certified != certified
            || their.report.residual != residual
            || their.report.predecessor != predecessor
        {
            return None;
        }
        let state = their.reconstructed.as_ref()?;
        let votes = their.residual.as_ref()?;
        if !their.is_usable() || !held.valid_membership(&their.report, state, votes) {
            return None;
        }
        Some((state, votes))
    }

    /// The reports whose certified state and residual object have both been
    /// reconstructed and checked against their signed roots: what an epoch
    /// proposal selects from. This node's own are usable by construction.
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
        let theirs = held
            .theirs
            .values()
            .filter(|their| their.is_usable())
            .filter_map(|their| {
                Some((
                    &their.report,
                    their.reconstructed.as_ref()?,
                    their.residual.as_ref()?,
                ))
            });
        own.chain(theirs)
            .filter(|(report, state, votes)| held.valid_membership(report, state, votes))
            .collect()
    }

    /// RAI: a report is taken only with a valid signature over its epoch,
    /// committee, both roots and the reporter. It is stored, not
    /// reconstructed: the contents are fetched when something has to be
    /// validated against them.
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
        if held.theirs.contains_key(&report.reporter) {
            return false;
        }
        // An empty residual object is settled by its signed root alone:
        // every replica knows the root of the empty object
        let residual = (report.residual == ResidualVotes::new().root()).then(ResidualVotes::new);
        held.theirs.insert(
            report.reporter,
            TheirReport {
                report,
                asked: None,
                first_asked: None,
                reconstructed: None,
                evidence: EvidenceState::Unchecked,
                transfer: None,
                residual,
                derived: None,
                working: None,
                sketch: None,
                sketched: None,
                cells: Sketch::MIN_CELLS,
            },
        );
        true
    }

    /// RAI: the tagged entries of a reconstructed report whose certificates
    /// are still to be checked here: every N and F entry the first time, the
    /// ones found missing afterwards, at most once per `RETRY_INTERVAL`. None
    /// while there is nothing to check or the report is verified.
    pub fn evidence_to_check(
        &self,
        epoch: ConsensusEpoch,
        reporter: &PublicKey,
        now: Timestamp,
    ) -> Option<Vec<BlockHash>> {
        let held = self.epochs.get(&epoch)?;
        let their = held.theirs.get(reporter)?;
        let state = their.reconstructed.as_ref()?;
        match &their.evidence {
            EvidenceState::Verified => None,
            EvidenceState::Unchecked => Some(
                state
                    .entries()
                    .filter(|(_, entry)| entry.status != CertifiedStatus::Recovery)
                    .map(|(block, _)| block.hash)
                    .collect(),
            ),
            EvidenceState::Missing { hashes, checked } => {
                (checked.elapsed(now) >= Self::RETRY_INTERVAL).then(|| hashes.clone())
            }
        }
    }

    /// RAI, "Reconstructing a report": check every N and F membership of a
    /// reconstructed report against the certificates the caller assembled
    /// from retained signed votes and against the verified predecessor. The
    /// report becomes usable only when nothing is missing; what is missing
    /// is returned so that its votes can be asked for. None while the
    /// predecessor is not known here: inherited finality can not be told
    /// from an unjustified tag without it.
    pub fn verify_evidence(
        &mut self,
        epoch: ConsensusEpoch,
        reporter: &PublicKey,
        certificates: &dyn CertificateSource,
        now: Timestamp,
    ) -> Option<Vec<BlockHash>> {
        let held = self.epochs.get_mut(&epoch)?;
        let (_, predecessor) = held.predecessor.as_ref()?;
        let their = held.theirs.get_mut(reporter)?;
        if their.evidence == EvidenceState::Verified {
            return Some(Vec::new());
        }
        let state = their.reconstructed.as_ref()?;
        let missing = unjustified_entries(epoch, state, predecessor, certificates);
        their.evidence = if missing.is_empty() {
            EvidenceState::Verified
        } else {
            EvidenceState::Missing {
                hashes: missing.clone(),
                checked: now,
            }
        };
        Some(missing)
    }

    /// RAI: whether the report's residual object is still to be derived:
    /// its certified state is reconstructed and its residual is not settled
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

    /// RAI: derive a report's residual object from the reporter's votes
    /// held here and the certified state reconstructed for it, by the rule
    /// the reporter itself applied, and accept it exactly when it hashes to
    /// the root the reporter signed. The votes were gossiped and checked
    /// when they arrived; when one was lost on the way the derived object
    /// misses the root; the derivation is retried as more of the reporter's
    /// votes arrive, and the object derived so far is kept for the diagnostics.
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
        let complete = derived.root() == their.report.residual && derived.first_evidence_complete();
        if complete {
            their.residual = Some(derived);
            their.working = None;
        } else {
            their.working = Some(derived);
        }
        Some(ReconcileResult {
            epoch,
            reporter,
            complete,
            entries: total,
            total,
        })
    }

    /// RAI: make a report's certified state usable: reconstructed from a
    /// difference. A state this node already holds that hashes to the
    /// signed root needs no message at all, which is the common case between
    /// validators that saw the same evidence; otherwise the request names
    /// the roots this node holds and a replica that knows one of them and the
    /// target answers. When no responder knows any of them, the retry adds a
    /// sketch of the live state (see `handle_ledger_sketch`). The residual
    /// object is derived, not fetched (see `derive_residual`).
    ///
    /// A request that goes unanswered is repeated after `RETRY_INTERVAL`,
    /// with the live root as it stands then.
    pub fn reconcile(
        &mut self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
        now: Timestamp,
    ) -> Option<(Vec<ReportMessage>, Option<ReconcileResult>)> {
        let held = self.epochs.get_mut(&epoch)?;
        let (target, needs_state) = {
            let their = held.theirs.get(&reporter)?;
            if their.is_complete() {
                return None;
            }
            (their.report.certified, their.reconstructed.is_none())
        };
        // A state already held that hashes to the target: nothing to ask for
        let known = if needs_state {
            held.state(target).cloned()
        } else {
            None
        };
        let sources = held.shared_sources();
        let live_root = held.live.root();
        let their = held.theirs.get_mut(&reporter)?;
        let mut messages = Vec::new();
        let mut result = None;
        let may_ask = their
            .asked
            .is_none_or(|asked| asked.elapsed(now) >= Self::RETRY_INTERVAL);
        // A root-based request already went unanswered: no responder knew a
        // source this node offered. The retry adds a sketch of the live
        // state, which any holder of the target can answer whatever its own
        // roots are (see `handle_ledger_sketch`).
        let sketch_due = their
            .first_asked
            .is_some_and(|first| first.elapsed(now) >= Self::SKETCH_INTERVAL)
            && their
                .sketched
                .is_none_or(|sent| sent.elapsed(now) >= Self::SKETCH_INTERVAL);

        if needs_state {
            if let Some(state) = known {
                let total = state.len();
                their.reconstructed = Some(state);
                their.sketch = None;
                result = Some(ReconcileResult {
                    epoch,
                    reporter,
                    complete: their.is_complete(),
                    entries: 0,
                    total,
                });
            } else if may_ask {
                messages.push(ReportMessage::Request(ReconReq {
                    epoch,
                    target,
                    sources,
                }));
                if sketch_due {
                    let cells = their.cells;
                    their.sketched = Some(now);
                    // The snapshot the sketch describes is kept until the
                    // pages arrive; a newer sketch replaces it
                    let base = held.live.clone();
                    let sketch = Sketch::over(base.digests().map(|(digest, ..)| digest), cells);
                    let their = held.theirs.get_mut(&reporter)?;
                    their.sketch = Some(LedgerSketch {
                        base,
                        pages: Vec::new(),
                    });
                    messages.push(ReportMessage::Sketch(LedgerSketchReq {
                        epoch,
                        target,
                        source: live_root,
                        cells: sketch.cells().iter().map(cell_wire).collect(),
                    }));
                }
            }
        }
        if !messages.is_empty() {
            let their = held.theirs.get_mut(&reporter)?;
            their.asked = Some(now);
            their.first_asked.get_or_insert(now);
        }
        Some((messages, result))
    }

    /// RAI, "Reconstructing a report": answer a sketch if this node holds
    /// the target: the report's own snapshot or one it reconstructed.
    /// Subtracting this node's sketch of the target leaves the symmetric
    /// difference; when it peels out, the entries the requester lacks go
    /// back in full and the digests of those it holds beyond the target go
    /// back as such, in pages. A difference too large for the cells is
    /// answered as incomplete: the requester enlarges its sketch. The
    /// requester accepts nothing before the rebuilt state hashes to the
    /// signed root.
    pub fn handle_ledger_sketch(
        &self,
        request: &LedgerSketchReq,
    ) -> Option<Vec<LedgerSketchReply>> {
        let held = self.epochs.get(&request.epoch)?;
        let target = held.state(request.target)?;
        if request.cells.is_empty() || request.cells.len() > LedgerSketchReq::MAX_CELLS {
            return None;
        }
        let incomplete = || {
            vec![LedgerSketchReply {
                epoch: request.epoch,
                target: request.target,
                source: request.source,
                incomplete: true,
                page: 0,
                pages: 1,
                added: Vec::new(),
                removed: Vec::new(),
            }]
        };
        let mut theirs = Sketch::from_cells(request.cells.iter().map(cell_of).collect());
        let mine = Sketch::over(
            target.digests().map(|(digest, ..)| digest),
            request.cells.len(),
        );
        if !theirs.subtract(&mine) {
            return None;
        }
        let Some(peeled) = theirs.peel() else {
            return Some(incomplete());
        };
        // `ours` are the requester's digests the target lacks, `theirs` the
        // target's entries the requester lacks
        let by_digest: HashMap<BlockHash, (CertifiedBlock, Certification)> = target
            .digests()
            .map(|(digest, block, entry)| (digest, (block, entry)))
            .collect();
        let mut added = Vec::with_capacity(peeled.theirs.len());
        for digest in &peeled.theirs {
            let Some((block, entry)) = by_digest.get(digest) else {
                // The requester's sketch does not describe a tagged ledger
                // set: nothing useful can be said
                return Some(incomplete());
            };
            added.push(CertifiedEntry {
                account: block.account,
                height: block.height,
                hash: block.hash,
                previous: entry.previous,
                status: entry.status.as_byte(),
            });
        }
        added.sort_by_key(|e| (e.account, e.height, e.hash, e.previous, e.status));
        let mut removed = peeled.ours;
        removed.sort();
        let total = removed.len() + added.len();
        let pages = total.div_ceil(LedgerSketchReply::MAX_ENTRIES).max(1);
        if pages > LedgerSketchReply::MAX_PAGES {
            return Some(incomplete());
        }
        Some(
            (0..pages)
                .map(|page| {
                    let offset = page * LedgerSketchReply::MAX_ENTRIES;
                    let end = (offset + LedgerSketchReply::MAX_ENTRIES).min(total);
                    LedgerSketchReply {
                        epoch: request.epoch,
                        target: request.target,
                        source: request.source,
                        incomplete: false,
                        page: page as u16,
                        pages: pages as u16,
                        removed: removed[offset.min(removed.len())..end.min(removed.len())]
                            .to_vec(),
                        added: added[offset.saturating_sub(removed.len())
                            ..end.saturating_sub(removed.len())]
                            .to_vec(),
                    }
                })
                .collect(),
        )
    }

    /// RAI: apply the pages of a sketched difference to the snapshot they
    /// name and accept the reconstruction exactly when the root comes out as
    /// the one the report signed; that check is the whole of the
    /// reconciliation. An incomplete answer enlarges the sketch for the
    /// next request.
    pub fn handle_ledger_sketch_reply(
        &mut self,
        reply: &LedgerSketchReply,
    ) -> Option<ReconcileResult> {
        let held = self.epochs.get_mut(&reply.epoch)?;
        let reporter = held
            .theirs
            .iter()
            .find(|(_, their)| {
                their.report.certified == reply.target
                    && their.reconstructed.is_none()
                    && their
                        .sketch
                        .as_ref()
                        .is_some_and(|sketch| sketch.base.root() == reply.source)
            })
            .map(|(reporter, _)| *reporter)?;
        let their = held.theirs.get_mut(&reporter)?;
        if reply.incomplete {
            their.cells = (their.cells * 4).min(Sketch::MAX_CELLS);
            their.sketch = None;
            return Some(ReconcileResult {
                epoch: reply.epoch,
                reporter,
                complete: false,
                entries: 0,
                total: 0,
            });
        }
        let count = reply.added.len() + reply.removed.len();
        if reply.pages == 0
            || reply.pages as usize > LedgerSketchReply::MAX_PAGES
            || reply.page >= reply.pages
            || count > LedgerSketchReply::MAX_ENTRIES
            || (reply.page + 1 < reply.pages && count != LedgerSketchReply::MAX_ENTRIES)
            || (reply.pages > 1 && count == 0)
            || reply
                .added
                .iter()
                .any(|e| CertifiedStatus::from_byte(e.status).is_none())
        {
            return None;
        }
        let sketch = their.sketch.as_mut()?;
        if sketch.pages.len() != reply.pages as usize {
            sketch.pages = vec![None; reply.pages as usize];
        }
        sketch.pages[reply.page as usize] = Some(reply.clone());
        if sketch.pages.iter().any(Option::is_none) {
            return Some(ReconcileResult {
                epoch: reply.epoch,
                reporter,
                complete: false,
                entries: count,
                total: sketch.base.len(),
            });
        }
        let sketch = their.sketch.take()?;
        let mut state = sketch.base;
        let by_digest: HashMap<BlockHash, CertifiedBlock> = state
            .digests()
            .map(|(digest, block, _)| (digest, block))
            .collect();
        let pages: Vec<_> = sketch.pages.into_iter().map(Option::unwrap).collect();
        for digest in pages.iter().flat_map(|p| &p.removed) {
            if let Some(block) = by_digest.get(digest) {
                state.remove(block);
            }
        }
        for entry in pages.iter().flat_map(|p| &p.added) {
            state.set(
                CertifiedBlock::new(entry.account, entry.height, entry.hash),
                Certification {
                    status: CertifiedStatus::from_byte(entry.status)?,
                    previous: entry.previous,
                },
            );
        }
        let complete = state.root() == reply.target;
        let total = state.len();
        if complete {
            their.reconstructed = Some(state);
        }
        Some(ReconcileResult {
            epoch: reply.epoch,
            reporter,
            complete: complete && their.is_complete(),
            entries: pages.iter().map(|p| p.added.len() + p.removed.len()).sum(),
            total,
        })
    }

    /// RAI: answer a request if this node knows the target and one of the
    /// sources offered. Either may be its live state, one it retained or a
    /// report it reconstructed; a replica that knows only one side does not
    /// answer, and no answer is not a verdict. Of the sources it knows, it
    /// takes the one closest to the target.
    ///
    /// Single-page convenience for the original small-difference tests.
    #[cfg(test)]
    pub fn handle_request(&self, request: &ReconReq) -> Result<ReconReply, ReconRefusal> {
        let pages = self.handle_request_pages(request)?;
        if pages.len() != 1 {
            return Err(ReconRefusal::TooLarge(
                pages.iter().map(|p| p.added.len() + p.removed.len()).sum(),
            ));
        }
        Ok(pages.into_iter().next().unwrap())
    }

    /// Split the immutable canonical difference into bounded pages. No empty
    /// source or full-target fallback is introduced; both roots must be known.
    pub fn handle_request_pages(
        &self,
        request: &ReconReq,
    ) -> Result<Vec<ReconReply>, ReconRefusal> {
        let target = self
            .epochs
            .get(&request.epoch)
            .and_then(|held| held.state(request.target))
            .ok_or(ReconRefusal::UnknownTarget)?;
        let held = &self.epochs[&request.epoch];
        let (source_root, source, delta) = request
            .sources
            .iter()
            .take(ReconReq::MAX_SOURCES)
            .filter_map(|root| {
                let source = held.state(*root)?;
                Some((*root, source, source.difference(target)))
            })
            .min_by_key(|(_, _, delta)| delta.len())
            .ok_or(ReconRefusal::UnknownSource)?;
        if delta.len() > ReconReply::MAX_ENTRIES * ReconReply::MAX_PAGES {
            return Err(ReconRefusal::TooLarge(delta.len()));
        }
        let entry = |block: &CertifiedBlock, held: &Certification| CertifiedEntry {
            account: block.account,
            height: block.height,
            hash: block.hash,
            previous: held.previous,
            status: held.status.as_byte(),
        };
        let removed: Vec<_> = delta
            .removed
            .iter()
            .filter_map(|block| Some(entry(block, &source.certification(block)?)))
            .collect();
        let added: Vec<_> = delta
            .added
            .iter()
            .map(|(block, held)| entry(block, held))
            .collect();
        let total = removed.len() + added.len();
        let pages = total.div_ceil(ReconReply::MAX_ENTRIES).max(1);
        Ok((0..pages)
            .map(|page| {
                let offset = page * ReconReply::MAX_ENTRIES;
                let end = (offset + ReconReply::MAX_ENTRIES).min(total);
                ReconReply {
                    epoch: request.epoch,
                    source: source_root,
                    target: request.target,
                    page: page as u16,
                    pages: pages as u16,
                    removed: removed[offset.min(removed.len())..end.min(removed.len())].to_vec(),
                    added: added
                        [offset.saturating_sub(removed.len())..end.saturating_sub(removed.len())]
                        .to_vec(),
                }
            })
            .collect())
    }

    /// RAI: apply a difference to the source it names and accept the
    /// reconstruction exactly when the root comes out as the one the report
    /// signed. That check is the whole of the reconciliation: the reply
    /// carries no signature of its own, and one that does not reach the
    /// root leaves the report unusable until an answer does.
    pub fn handle_reply(&mut self, reply: &ReconReply) -> Option<ReconcileResult> {
        let held = self.epochs.get_mut(&reply.epoch)?;
        let reporter = held
            .theirs
            .iter()
            .find(|(_, their)| {
                their.report.certified == reply.target && their.reconstructed.is_none()
            })
            .map(|(reporter, _)| *reporter)?;
        let count = reply.added.len() + reply.removed.len();
        if reply.pages == 0
            || reply.pages as usize > ReconReply::MAX_PAGES
            || reply.page >= reply.pages
            || count > ReconReply::MAX_ENTRIES
            || (reply.page + 1 < reply.pages && count != ReconReply::MAX_ENTRIES)
            || (reply.pages > 1 && count == 0)
            || reply
                .added
                .iter()
                .chain(&reply.removed)
                .any(|e| CertifiedStatus::from_byte(e.status).is_none())
        {
            return None;
        }
        let replace = held.theirs[&reporter]
            .transfer
            .as_ref()
            .is_none_or(|t| t.source != reply.source || t.pages.len() != reply.pages as usize);
        if replace {
            let base = held.state(reply.source)?.clone();
            held.theirs.get_mut(&reporter)?.transfer = Some(ReconTransfer {
                source: reply.source,
                base,
                pages: vec![None; reply.pages as usize],
            });
        }
        let their = held.theirs.get_mut(&reporter)?;
        let transfer = their.transfer.as_mut()?;
        transfer.pages[reply.page as usize] = Some(reply.clone());
        if transfer.pages.iter().any(Option::is_none) {
            return Some(ReconcileResult {
                epoch: reply.epoch,
                reporter,
                complete: false,
                entries: count,
                total: transfer.base.len(),
            });
        }
        let transfer = their.transfer.take()?;
        let mut state = transfer.base;
        let pages: Vec<_> = transfer.pages.into_iter().map(Option::unwrap).collect();
        // Apply all removals before additions, including status replacements
        // whose two edits straddle a page boundary.
        for entry in pages.iter().flat_map(|p| &p.removed) {
            state.remove(&CertifiedBlock::new(
                entry.account,
                entry.height,
                entry.hash,
            ));
        }
        for entry in pages.iter().flat_map(|p| &p.added) {
            state.set(
                CertifiedBlock::new(entry.account, entry.height, entry.hash),
                Certification {
                    status: CertifiedStatus::from_byte(entry.status)?,
                    previous: entry.previous,
                },
            );
        }
        let complete = state.root() == reply.target;
        let total = state.len();
        let their = held.theirs.get_mut(&reporter)?;
        if complete {
            their.reconstructed = Some(state);
        }
        Some(ReconcileResult {
            epoch: reply.epoch,
            reporter,
            complete: complete && their.is_complete(),
            entries: reply.added.len() + reply.removed.len(),
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
    /// RAI, "Reconstructing a report": what the reconstructed sets must
    /// satisfy besides hashing to the signed roots. The report binds the
    /// committee that issued the epoch's votes; one hash sits at one place;
    /// every R entry is a recovery-protected unresolved block of the verified
    /// predecessor the report is signed against; `G_i = V_i \ keys(T_i)` is
    /// exact, so no G hash is under any T tag; and every G hash has the
    /// reporter's own first vote behind it.
    fn valid_membership(
        &self,
        report: &Report,
        state: &CertifiedState,
        votes: &ResidualVotes,
    ) -> bool {
        if self
            .committee
            .is_some_and(|committee| committee != report.committee)
        {
            return false;
        }
        if !state.has_unique_hashes() || !votes.first_evidence_complete() {
            return false;
        }
        if votes
            .entries()
            .any(|(block, _, _)| state.contains_hash(&block.hash))
        {
            return false;
        }
        use crate::consensus::election::AccountSlot;
        state
            .entries()
            .filter(|(_, entry)| entry.status == CertifiedStatus::Recovery)
            .all(|(block, entry)| {
                self.predecessor.as_ref().is_some_and(|(hash, previous)| {
                    *hash == report.predecessor
                        && previous.valid_recovery_entry(
                            &AccountSlot::new(block.account, block.height),
                            block.hash,
                            entry.previous,
                        )
                })
            })
    }

    /// The states an epoch retains besides the live one: the report's own
    /// snapshot and the states this node passed through, which are what it
    /// can bridge from
    const MAX_HISTORY: usize = 8;

    /// A state this node knows by its root: the live one, one it retained,
    /// or a report it has reconstructed. The reconstructed ones are what let
    /// it bridge between two reporters for a third replica.
    fn state(&self, root: BlockHash) -> Option<&CertifiedState> {
        if self.live.root() == root {
            return Some(&self.live);
        }
        if let Some(state) = self.history.get(&root) {
            return Some(state);
        }
        self.theirs
            .values()
            .filter_map(|their| their.reconstructed.as_ref())
            .find(|state| state.root() == root)
    }

    /// The roots this node can offer as difference sources: the ones its own
    /// reports signed, the reports it has reconstructed, and its live state.
    /// The signed and reconstructed ones are states other replicas hold too;
    /// the live root is shared only once the states have converged, so it
    /// is offered last.
    fn shared_sources(&self) -> Vec<BlockHash> {
        let mut roots: Vec<BlockHash> = self
            .signed
            .iter()
            .map(|report| report.certified)
            .chain(
                self.theirs
                    .values()
                    .filter(|their| their.reconstructed.is_some())
                    .map(|their| their.report.certified),
            )
            .collect();
        roots.sort();
        roots.dedup();
        roots.truncate(ReconReq::MAX_SOURCES - 1);
        roots.push(self.live.root());
        roots
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

fn cell_wire(cell: &SketchCell) -> SketchCellWire {
    SketchCellWire {
        count: cell.count,
        key: cell.key,
        check: cell.check,
    }
}

fn cell_of(cell: &SketchCellWire) -> SketchCell {
    SketchCell {
        count: cell.count,
        key: cell.key,
        check: cell.check,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::Account;
    use std::time::Duration;

    #[test]
    fn matching_g_hashes_without_first_vote_evidence_are_not_usable() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let t = CertifiedState::new();
        let mut g = ResidualVotes::new();
        g.record(block(20), parent(20), ResidualKind::First);
        let mut exchange = ReportExchange::new();
        exchange.report_epoch(
            epoch,
            t.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(exchange.handle_report(signed_with(&key, epoch, &t, &g)));
        exchange
            .reconcile(epoch, key.public_key(), later())
            .unwrap();
        let only_final = vec![(block(20), ResidualKind::Final, parent(20))];
        assert_eq!(
            ResidualVotes::derive(&t, only_final.clone()).root(),
            g.root()
        );
        assert!(
            !exchange
                .derive_residual(epoch, key.public_key(), only_final, later())
                .unwrap()
                .complete
        );
        assert!(theirs_usable(&mut exchange, epoch).is_empty());
        assert!(
            exchange
                .derive_residual(epoch, key.public_key(), g.entries(), later())
                .unwrap()
                .complete
        );
        assert_eq!(theirs_usable(&mut exchange, epoch).len(), 1);
    }

    /// RAI, "Reconstructing a report": a reconstructed root authenticates
    /// the tags, not the quorums behind them. A report is usable only once
    /// every N and F entry has its certificate here, and what is missing is
    /// what the exchange asks the votes for.
    #[test]
    fn a_reconstructed_report_is_not_usable_until_its_certificates_are_justified() {
        use crate::consensus::election::CertificateKinds;
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let mut frozen = state_of(0..3);
        frozen.certify(block(2), parent(2), CertifiedStatus::Finalized);
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            frozen.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &frozen)));
        // Nothing to check before the state is reconstructed
        assert!(
            ours.evidence_to_check(epoch, &key.public_key(), later())
                .is_none()
        );
        ours.reconcile(epoch, key.public_key(), later()).unwrap();
        let mut to_check = ours
            .evidence_to_check(epoch, &key.public_key(), later())
            .unwrap();
        to_check.sort();
        let mut expected: Vec<_> = (0..3).map(|i| block(i).hash).collect();
        expected.sort();
        assert_eq!(to_check, expected);
        // Without the predecessor nothing can be verified
        assert!(
            ours.verify_evidence(epoch, &key.public_key(), &AllCertified, later())
                .is_none()
        );
        let genesis = crate::consensus::election::EpochLedger::new();
        ours.set_predecessor(epoch, std::sync::Arc::new(genesis));
        // Reconstructed, but not usable: no certificate is assembled here
        let none: HashMap<(ConsensusEpoch, BlockHash), CertificateKinds> = HashMap::new();
        let checked = later();
        let missing = ours
            .verify_evidence(epoch, &key.public_key(), &none, checked)
            .unwrap();
        assert_eq!(missing.len(), 3);
        assert_eq!(ours.usable(epoch).len(), 1, "own report only");
        assert!(
            ours.evidence_to_check(epoch, &key.public_key(), checked)
                .is_none(),
            "not rechecked before the retry interval"
        );
        assert_eq!(
            ours.evidence_to_check(epoch, &key.public_key(), later())
                .unwrap()
                .len(),
            3
        );
        // The votes arrive for two of them: the third is still missing
        let nc_only = CertificateKinds {
            nc: true,
            fc: false,
            ff: false,
        };
        let mut some = HashMap::new();
        for i in 0..2 {
            some.insert((epoch, block(i).hash), nc_only);
        }
        let missing = ours
            .verify_evidence(epoch, &key.public_key(), &some, later())
            .unwrap();
        assert_eq!(missing, vec![block(2).hash]);
        assert_eq!(ours.usable(epoch).len(), 1);
        // An NC does not justify F; a final certificate does
        some.insert((epoch, block(2).hash), nc_only);
        assert_eq!(
            ours.verify_evidence(epoch, &key.public_key(), &some, later())
                .unwrap(),
            vec![block(2).hash]
        );
        some.insert(
            (epoch, block(2).hash),
            CertificateKinds {
                nc: true,
                fc: true,
                ff: false,
            },
        );
        assert!(
            ours.verify_evidence(epoch, &key.public_key(), &some, later())
                .unwrap()
                .is_empty()
        );
        assert_eq!(ours.usable(epoch).len(), 2);
        assert!(
            ours.evidence_to_check(epoch, &key.public_key(), later())
                .is_none()
        );
    }

    /// RAI: a report must bind the committee that issued the epoch's votes,
    /// and its G must be exactly the reporter's votes outside T: a hash that
    /// is under a T tag and in G makes the report unusable
    #[test]
    fn a_report_of_another_committee_or_with_a_g_hash_in_t_is_not_usable() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let t = state_of(0..3);
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            t.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &t)));
        ours.reconcile(epoch, key.public_key(), later()).unwrap();
        assert_eq!(theirs_usable(&mut ours, epoch).len(), 1);
        ours.set_committee(epoch, BlockHash::from(8));
        assert!(theirs_usable(&mut ours, epoch).is_empty());
        ours.set_committee(epoch, BlockHash::from(7));
        assert_eq!(theirs_usable(&mut ours, epoch).len(), 1);

        // A G that names a hash of T: not the exact difference
        let mut g = ResidualVotes::new();
        g.record(block(1), parent(1), ResidualKind::First);
        let key = PrivateKey::from(3);
        let mut overlapping = ReportExchange::new();
        overlapping.report_epoch(
            epoch,
            t.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(overlapping.handle_report(signed_with(&key, epoch, &t, &g)));
        overlapping
            .reconcile(epoch, key.public_key(), later())
            .unwrap();
        // The honest derivation is exact, so the object never reaches the
        // signed root; the membership check would refuse it even if it did
        assert!(
            !overlapping
                .derive_residual(epoch, key.public_key(), g.entries(), later())
                .unwrap()
                .complete
        );
        assert!(theirs_usable(&mut overlapping, epoch).is_empty());
        assert!(!overlapping.epochs[&epoch].valid_membership(
            &signed_with(&key, epoch, &t, &g),
            &t,
            &g
        ));
    }

    #[test]
    fn r_membership_needs_the_matching_verified_predecessor() {
        use crate::consensus::election::EpochLedger;
        use std::sync::Arc;
        let b = block(1);
        let previous = Arc::new(
            EpochLedger::from_checkpoint_entries(&[CertifiedEntry {
                account: b.account,
                height: b.height,
                hash: b.hash,
                previous: parent(1),
                status: 3,
            }])
            .unwrap(),
        );
        let mut exchange = ReportExchange::new();
        exchange.report_epoch(
            ConsensusEpoch::ZERO,
            previous.report_ledger(),
            ResidualVotes::new(),
            BlockHash::from(7),
            previous.state_hash(),
            &[PrivateKey::from(1)],
        );
        assert!(exchange.usable(ConsensusEpoch::ZERO).is_empty());
        exchange.set_predecessor(ConsensusEpoch::ZERO, previous.clone());
        assert_eq!(exchange.usable(ConsensusEpoch::ZERO).len(), 1);
        exchange.set_predecessor(ConsensusEpoch::ZERO, Arc::new(EpochLedger::new()));
        assert!(exchange.usable(ConsensusEpoch::ZERO).is_empty());
    }

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
            BlockHash::ZERO,
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
                    BlockHash::ZERO,
                    &keys,
                )
                .is_empty()
        );
        assert_eq!(exchange.live_root(ConsensusEpoch::ZERO), Some(root));
    }

    /// The reporter's signature binds both roots
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
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &theirs)));

        let (messages, result) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        assert!(messages.is_empty(), "nothing to ask for");
        let result = result.expect("complete at once");
        assert!(result.complete);
        assert_eq!(result.entries, 0);
        assert_eq!(theirs_usable(&mut ours, epoch).len(), 1);
        // And it is not reconciled twice
        assert!(ours.reconcile(epoch, key.public_key(), later()).is_none());
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
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &theirs)));

        let request = request_of(&mut ours, epoch, key.public_key());
        assert!(request.sources.contains(&state_of(0..35).root()));

        // A validator that reported the same 35 and has since seen the other
        // five knows both the source and the target
        let mut bridging = ReportExchange::new();
        bridging.report_epoch(
            epoch,
            state_of(0..35),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(3)],
        );
        bridging.refresh_live(epoch, state_of(0..40));
        let reply = bridging.handle_request(&request).expect("it knows both");
        assert_eq!(reply.source, state_of(0..35).root());
        assert_eq!(reply.added.len(), 5);
        assert!(reply.removed.is_empty());

        let result = ours.handle_reply(&reply).expect("a reply we asked for");
        assert!(result.complete);
        assert_eq!(result.entries, 5);
        assert_eq!(result.total, 40);
        let usable = theirs_usable(&mut ours, epoch);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].0, theirs.root());
    }

    /// RAI: "Edits may add, remove, or change the status of records." A
    /// live state has grown past the root its report froze, so bridging
    /// back to that root drops what the report predates.
    #[test]
    fn a_difference_removes_what_the_report_predates() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let theirs = state_of(0..10);
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            state_of(0..12),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &theirs)));
        let request = request_of(&mut ours, epoch, key.public_key());

        // The reporter's live state has grown to what the requester holds
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            epoch,
            theirs.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[key.clone()],
        );
        reporter.refresh_live(epoch, state_of(0..12));
        let reply = reporter.handle_request(&request).expect("it knows both");
        assert!(reply.added.is_empty());
        assert_eq!(
            reply.removed.len(),
            2,
            "the two entries the report predates"
        );

        assert!(ours.handle_reply(&reply).unwrap().complete);
        assert_eq!(
            theirs_usable(&mut ours, epoch),
            vec![(theirs.root(), ResidualVotes::new().root())]
        );
    }

    /// A validator whose live state has moved on can still bridge from the
    /// state it was at, because it retains the states it passed through.
    /// That is what makes a request answerable after the fact.
    #[test]
    fn a_state_left_behind_still_bridges() {
        let epoch = ConsensusEpoch::ZERO;
        let mut exchange = ReportExchange::new();
        exchange.report_epoch(
            epoch,
            state_of(0..10),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
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
                    target: exchange.live_root(epoch).unwrap(),
                    sources: vec![source],
                })
                .expect("both states are known");
            assert!(!reply.added.is_empty());
        }
        // And the live state bridges back to the report, which is what a
        // validator asking for the historical root needs
        let reply = exchange
            .handle_request(&ReconReq {
                epoch,
                target: reported,
                sources: vec![exchange.live_root(epoch).unwrap()],
            })
            .unwrap();
        assert!(reply.added.is_empty());
        assert_eq!(reply.removed.len(), 20);
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
            BlockHash::ZERO,
            &[PrivateKey::from(1)],
        );
        let reported = exchange.live_root(epoch).unwrap();
        for i in 6..40 {
            exchange.refresh_live(epoch, state_of(0..i));
        }
        let reply = exchange.handle_request(&ReconReq {
            epoch,
            target: exchange.live_root(epoch).unwrap(),
            sources: vec![reported],
        });
        assert!(reply.is_ok(), "the reported state is still known");
    }

    /// A replica that does not know both states returns nothing, which the
    /// requester treats as no answer rather than as a verdict. A request
    /// offers several sources, and the first one known is taken.
    #[test]
    fn a_replica_that_knows_one_state_does_not_answer() {
        let epoch = ConsensusEpoch::ZERO;
        let mut exchange = ReportExchange::new();
        exchange.report_epoch(
            epoch,
            state_of(0..10),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(1)],
        );
        let known = exchange.live_root(epoch).unwrap();
        let unknown = BlockHash::from(12345);
        for (sources, target) in [
            (vec![known], unknown),
            (vec![unknown], known),
            (vec![unknown, BlockHash::from(6789)], known),
        ] {
            assert!(
                exchange
                    .handle_request(&ReconReq {
                        epoch,
                        target,
                        sources,
                    })
                    .is_err()
            );
        }
        // One known source among unknown ones: the difference is empty
        let reply = exchange
            .handle_request(&ReconReq {
                epoch,
                target: known,
                sources: vec![unknown, known],
            })
            .unwrap();
        assert_eq!(reply.source, known);
        assert!(reply.added.is_empty() && reply.removed.is_empty());
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
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &state_of(0..10))));
        let request = request_of(&mut ours, epoch, key.public_key());

        // One entry short of the target
        let reply = ReconReply {
            page: 0,
            pages: 1,
            epoch,
            source: state_of(0..8).root(),
            target: request.target,
            added: vec![CertifiedEntry {
                account: block(8).account,
                height: block(8).height,
                hash: block(8).hash,
                previous: parent(8),
                status: 0,
            }],
            removed: Vec::new(),
        };
        let result = ours.handle_reply(&reply).unwrap();
        assert!(!result.complete);
        assert!(theirs_usable(&mut ours, epoch).is_empty());
        // The request is repeated until an answer reaches the root
        assert!(
            !request_of(&mut ours, epoch, key.public_key())
                .sources
                .is_empty()
        );
    }

    #[test]
    fn paged_difference_survives_reordering_duplicates_and_retries() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let source = state_of(0..1300);
        let target = state_of(1000..1601);
        let mut responder = ReportExchange::new();
        responder.report_epoch(
            epoch,
            target.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[key.clone()],
        );
        responder.refresh_live(epoch, source.clone());
        let mut receiver = ReportExchange::new();
        receiver.refresh_live(epoch, source.clone());
        receiver.handle_report(signed(&key, epoch, &target));
        let pages = responder
            .handle_request_pages(&ReconReq {
                epoch,
                target: target.root(),
                sources: vec![source.root()],
            })
            .unwrap();
        assert_eq!(pages.len(), 3);
        assert!(
            pages
                .iter()
                .all(|p| p.added.len() + p.removed.len() <= ReconReply::MAX_ENTRIES)
        );
        assert!(!receiver.handle_reply(&pages[2]).unwrap().complete);
        assert!(!receiver.handle_reply(&pages[2]).unwrap().complete);
        assert!(!receiver.handle_reply(&pages[0]).unwrap().complete);
        assert!(theirs_usable(&mut receiver, epoch).is_empty());
        let mut corrupted = pages[1].clone();
        corrupted.removed[0].hash = BlockHash::from(999999);
        assert!(!receiver.handle_reply(&corrupted).unwrap().complete);
        assert!(theirs_usable(&mut receiver, epoch).is_empty());
        for page in &pages {
            receiver.handle_reply(page);
        }
        assert_eq!(theirs_usable(&mut receiver, epoch).len(), 1);
        assert_eq!(
            receiver.epochs[&epoch].theirs[&key.public_key()]
                .reconstructed
                .as_ref()
                .unwrap()
                .root(),
            target.root()
        );
    }

    /// The live states of correct validators converge to the union of the
    /// epoch's certificates, which is how a report is reached in the first
    /// place: its reporter knows that union and the root it signed. A
    /// reconstructed report is then a state its holder knows too, offered
    /// as a source and served as a bridge to a third replica, which reaches
    /// the second report in the two entries the reports differ in.
    #[test]
    fn a_reconstructed_report_is_the_shared_source_for_the_next_one() {
        let epoch = ConsensusEpoch::ZERO;
        let first_key = PrivateKey::from(1);
        let second_key = PrivateKey::from(2);
        let first_state = state_of(0..30);
        let mut second_state = state_of(0..30);
        second_state.remove(&block(0));
        second_state.certify(block(30), parent(30), CertifiedStatus::Notarized);
        // What every live state grows to
        let mut union = state_of(0..31);
        for i in 200..204 {
            union.certify(block(i), parent(i), CertifiedStatus::Notarized);
        }
        let report = |exchange: &mut ReportExchange, key: &PrivateKey, state: &CertifiedState| {
            exchange.report_epoch(
                epoch,
                state.clone(),
                ResidualVotes::new(),
                BlockHash::from(7),
                BlockHash::ZERO,
                &[key.clone()],
            );
            exchange.refresh_live(epoch, union.clone());
        };
        let mut first_reporter = ReportExchange::new();
        report(&mut first_reporter, &first_key, &first_state);
        let mut second_reporter = ReportExchange::new();
        report(&mut second_reporter, &second_key, &second_state);
        let mut ours = ReportExchange::new();
        report(&mut ours, &PrivateKey::from(8), &state_of(200..204));

        // The second reporter reaches the first report through the union
        assert!(second_reporter.handle_report(signed(&first_key, epoch, &first_state)));
        let request = request_of(&mut second_reporter, epoch, first_key.public_key());
        let reply = first_reporter.handle_request(&request).unwrap();
        assert_eq!(reply.source, union.root());
        assert!(second_reporter.handle_reply(&reply).unwrap().complete);

        // So does the requester
        assert!(ours.handle_report(signed(&first_key, epoch, &first_state)));
        assert!(ours.handle_report(signed(&second_key, epoch, &second_state)));
        let request = request_of(&mut ours, epoch, first_key.public_key());
        let reply = first_reporter.handle_request(&request).unwrap();
        assert_eq!(reply.source, union.root());
        assert_eq!(reply.added.len() + reply.removed.len(), 5);
        assert!(ours.handle_reply(&reply).unwrap().complete);

        // Now the first report's root is a source it offers too. The second
        // reporter knows it, and answers from it rather than from the
        // union: two edits instead of five
        let request = request_of(&mut ours, epoch, second_key.public_key());
        assert!(request.sources.contains(&first_state.root()));
        let reply = second_reporter.handle_request(&request).unwrap();
        assert_eq!(reply.source, first_state.root());
        assert_eq!(reply.added.len(), 1);
        assert_eq!(reply.removed.len(), 1);
        assert!(ours.handle_reply(&reply).unwrap().complete);
        assert_eq!(theirs_usable(&mut ours, epoch).len(), 2);
    }

    /// RAI: "A report is usable only after reconstructing T_i and G_i". The
    /// residual object is derived from the reporter's votes this node
    /// received, against the certified state reconstructed for the report:
    /// what that state summarizes is left out, and the rest has to hash to
    /// the signed root. A vote not received here leaves the report
    /// unusable until it is.
    #[test]
    fn a_residual_object_is_derived_from_the_reporters_votes() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let mut theirs = state_of(0..10);
        theirs.certify(block(3), parent(3), CertifiedStatus::Finalized);
        // The reporter's votes: summarized ones and residual ones
        let votes = vec![
            (block(3), ResidualKind::First, parent(3)),
            (block(3), ResidualKind::Final, parent(3)),
            (block(5), ResidualKind::First, parent(5)),
            (block(5), ResidualKind::Final, parent(5)),
            (block(20), ResidualKind::First, parent(20)),
            (block(21), ResidualKind::First, parent(21)),
        ];
        let residual = ResidualVotes::derive(&theirs, votes.clone());
        assert_eq!(
            residual.len(),
            2,
            "only hashes outside every T tag remain in G"
        );

        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            epoch,
            theirs.clone(),
            residual.clone(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[key.clone()],
        );
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            theirs.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed_with(&key, epoch, &theirs, &residual)));

        // The certified state matches at once, but the report is not usable
        let (messages, result) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        assert!(messages.is_empty());
        assert!(!result.expect("the state matched").complete);
        assert!(theirs_usable(&mut ours, epoch).is_empty());
        let start = later();
        assert!(ours.needs_residual(epoch, &key.public_key(), start));

        // One vote short: no object, and not tried again before the interval
        let short = ours
            .derive_residual(epoch, key.public_key(), votes[..5].to_vec(), start)
            .unwrap();
        assert!(!short.complete);
        assert_eq!(short.total, 1);
        assert!(theirs_usable(&mut ours, epoch).is_empty());
        assert!(!ours.needs_residual(epoch, &key.public_key(), start));
        assert!(ours.needs_residual(
            epoch,
            &key.public_key(),
            start + ReportExchange::RETRY_INTERVAL
        ));

        // Every vote, in another order: derived and usable
        let mut shuffled = votes.clone();
        shuffled.reverse();
        let done = ours
            .derive_residual(epoch, key.public_key(), shuffled, later())
            .unwrap();
        assert!(done.complete);
        assert_eq!(done.total, 2);
        let usable = theirs_usable(&mut ours, epoch);
        assert_eq!(usable, vec![(theirs.root(), residual.root())]);
        assert!(!ours.needs_residual(epoch, &key.public_key(), later()));
        assert!(
            ours.derive_residual(epoch, key.public_key(), votes, later())
                .is_none()
        );
    }

    /// A vote lost on the way leaves the derivation one record short of the
    /// signed root. A sketch of the derived object goes to the reporter,
    /// which peels the difference out and sends the record back, and the
    /// object then hashes to the root. A record derived here that the
    /// reporter did not commit to goes the other way.
    /// The failure observed in the fork diagnostics: a lagging replica's
    /// live projection never matches any root the reporters hold, so its
    /// root-based requests are refused by everyone. The retry sketches the
    /// live state instead, a holder of the frozen state answers with the
    /// difference, and it is accepted only because the rebuilt state hashes
    /// to the signed root.
    #[test]
    fn a_report_is_reconstructed_by_a_sketch_when_no_root_is_shared() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let frozen = state_of(0..12);
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            epoch,
            frozen.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[key.clone()],
        );
        // The lagging replica lacks 10 and 11, holds 12 and 13 of its own,
        // and has finalized 9 where the reporter froze it notarized
        let mut live = state_of(0..10);
        live.certify(block(12), parent(12), CertifiedStatus::Notarized);
        live.certify(block(13), parent(13), CertifiedStatus::Notarized);
        live.certify(block(9), parent(9), CertifiedStatus::Finalized);
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            live.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &frozen)));

        // The first attempt names roots only, and nobody knows them
        let (messages, _) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        assert_eq!(messages.len(), 1);
        let ReportMessage::Request(request) = &messages[0] else {
            panic!("a root-based request first");
        };
        assert_eq!(
            reporter.handle_request_pages(request),
            Err(ReconRefusal::UnknownSource)
        );
        assert!(theirs_usable(&mut ours, epoch).is_empty());

        // The retry adds a sketch of the live state
        let (messages, _) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        let sketch = messages
            .iter()
            .find_map(|message| match message {
                ReportMessage::Sketch(sketch) => Some(sketch.clone()),
                _ => None,
            })
            .expect("a sketch on retry");
        assert_eq!(sketch.target, frozen.root());
        assert_eq!(sketch.source, live.root());
        assert_eq!(sketch.cells.len(), Sketch::MIN_CELLS);
        // A replica without the target does not answer
        assert!(ours.handle_ledger_sketch(&sketch).is_none());
        let replies = reporter.handle_ledger_sketch(&sketch).unwrap();
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].incomplete);
        // 10, 11, and 9 as notarized come in; 12, 13 and 9 as finalized go
        assert_eq!(replies[0].added.len(), 3);
        assert_eq!(replies[0].removed.len(), 3);
        let done = ours.handle_ledger_sketch_reply(&replies[0]).unwrap();
        assert!(done.complete);
        assert_eq!(done.entries, 6);
        assert_eq!(done.total, 12);
        assert_eq!(
            theirs_usable(&mut ours, epoch),
            vec![(frozen.root(), ResidualVotes::new().root())]
        );
        // And this node serves the state it reconstructed in turn
        assert!(ours.handle_ledger_sketch(&sketch).is_some());
    }

    /// A difference too large for the sketch is answered as incomplete and
    /// the next sketch is larger; a difference too large for one message
    /// arrives in pages, in any order and with repeats
    #[test]
    fn a_sketched_difference_grows_the_sketch_and_arrives_in_pages() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let frozen = state_of(0..900);
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            epoch,
            frozen.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[key.clone()],
        );
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            state_of(0..300),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &frozen)));
        ours.reconcile(epoch, key.public_key(), later()).unwrap();
        let mut rounds = 0;
        loop {
            let (messages, _) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
            let sketch = messages
                .into_iter()
                .find_map(|message| match message {
                    ReportMessage::Sketch(sketch) => Some(sketch),
                    _ => None,
                })
                .expect("a sketch on every retry");
            let replies = reporter.handle_ledger_sketch(&sketch).unwrap();
            rounds += 1;
            assert!(rounds <= 4, "the sketch grows to the difference");
            if replies[0].incomplete {
                assert!(
                    !ours
                        .handle_ledger_sketch_reply(&replies[0])
                        .unwrap()
                        .complete
                );
                continue;
            }
            assert_eq!(replies.len(), 2);
            assert!(
                !ours
                    .handle_ledger_sketch_reply(&replies[1])
                    .unwrap()
                    .complete
            );
            assert!(
                !ours
                    .handle_ledger_sketch_reply(&replies[1])
                    .unwrap()
                    .complete
            );
            let done = ours.handle_ledger_sketch_reply(&replies[0]).unwrap();
            assert!(done.complete);
            assert_eq!(done.entries, 600);
            assert_eq!(done.total, 900);
            break;
        }
        assert!(rounds > 1);
        assert_eq!(theirs_usable(&mut ours, epoch).len(), 1);
    }

    /// A reply naming a snapshot this node no longer sketches is ignored,
    /// and a reply that does not reach the root leaves nothing behind
    #[test]
    fn a_sketch_reply_for_another_snapshot_or_root_is_not_applied() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let frozen = state_of(0..12);
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            epoch,
            frozen.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[key.clone()],
        );
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            state_of(0..10),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &frozen)));
        ours.reconcile(epoch, key.public_key(), later()).unwrap();
        let (messages, _) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        let sketch = messages
            .into_iter()
            .find_map(|message| match message {
                ReportMessage::Sketch(sketch) => Some(sketch),
                _ => None,
            })
            .unwrap();
        let mut replies = reporter.handle_ledger_sketch(&sketch).unwrap();
        // The live state moved on: a new sketch replaces the snapshot
        ours.refresh_live(epoch, state_of(0..11));
        ours.reconcile(epoch, key.public_key(), later()).unwrap();
        assert!(ours.handle_ledger_sketch_reply(&replies[0]).is_none());
        // A tampered page rebuilds a state that misses the root
        let (messages, _) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        let sketch = messages
            .into_iter()
            .find_map(|message| match message {
                ReportMessage::Sketch(sketch) => Some(sketch),
                _ => None,
            })
            .unwrap();
        replies = reporter.handle_ledger_sketch(&sketch).unwrap();
        replies[0].added[0].status = CertifiedStatus::Finalized.as_byte();
        assert!(
            !ours
                .handle_ledger_sketch_reply(&replies[0])
                .unwrap()
                .complete
        );
        assert!(theirs_usable(&mut ours, epoch).is_empty());
        // The untampered pages still do
        let replies = reporter.handle_ledger_sketch(&sketch).unwrap();
        assert!(
            !ours
                .handle_ledger_sketch_reply(&replies[0])
                .is_some_and(|r| r.complete)
        );
        let (messages, _) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        let sketch = messages
            .into_iter()
            .find_map(|message| match message {
                ReportMessage::Sketch(sketch) => Some(sketch),
                _ => None,
            })
            .unwrap();
        let replies = reporter.handle_ledger_sketch(&sketch).unwrap();
        assert!(
            ours.handle_ledger_sketch_reply(&replies[0])
                .unwrap()
                .complete
        );
        assert_eq!(theirs_usable(&mut ours, epoch).len(), 1);
    }

    /// The derivation waits for the certified state: without it, what the
    /// inventory summarizes is unknown
    #[test]
    fn the_residual_is_not_derived_before_the_certified_state() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let theirs = state_of(0..10);
        let mut residual = ResidualVotes::new();
        residual.record(block(20), parent(20), ResidualKind::First);
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            state_of(0..8),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed_with(&key, epoch, &theirs, &residual)));
        assert!(!ours.needs_residual(epoch, &key.public_key(), later()));
        assert!(
            ours.derive_residual(
                epoch,
                key.public_key(),
                vec![(block(20), ResidualKind::First, parent(20))],
                later()
            )
            .is_none()
        );
    }

    /// A residual object that is empty needs no derivation at all: its
    /// signed root is the root of the empty object, which every replica knows
    #[test]
    fn an_empty_residual_object_needs_no_derivation() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let theirs = state_of(0..10);
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            theirs.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &theirs)));

        let (messages, result) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        assert!(messages.is_empty());
        assert!(result.unwrap().complete);
        assert_eq!(theirs_usable(&mut ours, epoch).len(), 1);
    }

    /// RAI: a proposal selects `N - f` reports from distinct old-committee
    /// validators, and the reporter is one of them. A validator that left
    /// its own out would be short of its own weight, which the largest
    /// member of a committee can not make up from the rest.
    #[test]
    fn a_validators_own_report_is_usable_to_itself() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let certified = state_of(0..10);
        let mut residual = ResidualVotes::new();
        residual.record(block(20), parent(20), ResidualKind::First);

        let mut exchange = ReportExchange::new();
        exchange.report_epoch(
            epoch,
            certified.clone(),
            residual.clone(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[key.clone()],
        );
        // The live state moves on; the report's own snapshot is still usable
        exchange.refresh_live(epoch, state_of(0..14));

        let usable = exchange.usable(epoch);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].0.reporter, key.public_key());
        assert_eq!(usable[0].1.root(), certified.root());
        let signed_root = residual.root();
        assert_eq!(usable[0].2.root(), signed_root);

        let named = exchange
            .usable_report(
                epoch,
                &key.public_key(),
                certified.root(),
                signed_root,
                BlockHash::ZERO,
            )
            .expect("a proposal may name it");
        assert_eq!(named.0.root(), certified.root());
        assert_eq!(named.1.first_votes().count(), 1);

        // A proposal naming the wrong roots, or another predecessor, for
        // this reporter is refused
        assert!(
            exchange
                .usable_report(
                    epoch,
                    &key.public_key(),
                    BlockHash::from(9),
                    signed_root,
                    BlockHash::ZERO,
                )
                .is_none()
        );
        assert!(
            exchange
                .usable_report(
                    epoch,
                    &key.public_key(),
                    certified.root(),
                    signed_root,
                    BlockHash::from(1),
                )
                .is_none()
        );
    }

    /// A request that went unanswered is repeated only after the retry
    /// interval, with the sources as they stand then
    #[test]
    fn an_unanswered_request_is_repeated_after_the_retry_interval() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            state_of(0..8),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &state_of(0..10))));
        let start = Timestamp::new_test_instance();

        let (first, _) = ours.reconcile(epoch, key.public_key(), start).unwrap();
        assert_eq!(first.len(), 1);
        let (soon, _) = ours
            .reconcile(epoch, key.public_key(), start + Duration::from_millis(100))
            .unwrap();
        assert!(soon.is_empty(), "not asked again yet");
        let (again, _) = ours
            .reconcile(
                epoch,
                key.public_key(),
                start + ReportExchange::RETRY_INTERVAL,
            )
            .unwrap();
        assert_eq!(again.len(), 1);
    }

    /// The signed reports are broadcast again at the repeat interval, for
    /// every epoch still held
    #[test]
    fn own_reports_are_repeated() {
        let mut exchange = ReportExchange::new();
        let key = PrivateKey::from(1);
        let start = Timestamp::new_test_instance();
        for epoch in 0..2 {
            exchange.report_epoch(
                ConsensusEpoch::new(epoch),
                state_of(0..3),
                ResidualVotes::new(),
                BlockHash::from(7),
                BlockHash::ZERO,
                &[key.clone()],
            );
        }
        assert_eq!(exchange.repeat_reports(start).len(), 2);
        assert!(
            exchange
                .repeat_reports(start + Duration::from_millis(500))
                .is_empty()
        );
        let repeated = exchange.repeat_reports(start + ReportExchange::REPEAT_INTERVAL);
        assert_eq!(repeated.len(), 2);
        assert!(
            repeated
                .iter()
                .all(|m| matches!(m, ReportMessage::Broadcast(_)))
        );
    }

    #[test]
    fn canonical_projection_removes_stale_entries_but_preserves_signed_snapshots() {
        let epoch = ConsensusEpoch::ZERO;
        let mut exchange = ReportExchange::new();
        let frozen = state_of(0..12);
        exchange.report_epoch(
            epoch,
            frozen.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            BlockHash::ZERO,
            &[PrivateKey::from(1)],
        );
        let mut selected = state_of(0..10);
        selected.certify(block(3), parent(3), CertifiedStatus::Finalized);
        exchange.refresh_live(epoch, selected.clone());
        assert_eq!(exchange.live_root(epoch), Some(selected.root()));
        let reply = exchange
            .handle_request(&ReconReq {
                epoch,
                target: frozen.root(),
                sources: vec![selected.root()],
            })
            .unwrap();
        assert_eq!(reply.target, frozen.root());
        assert_eq!(reply.added.len(), 3); // two removed positions + old status
        assert_eq!(exchange.own_reports(epoch)[0].certified, frozen.root());
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
                BlockHash::ZERO,
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

    /// The parent a test block names, distinct per block so that a state
    /// commits to it
    fn parent(i: u64) -> BlockHash {
        BlockHash::from(i * 13 + 2)
    }

    fn state_of(blocks: std::ops::Range<u64>) -> CertifiedState {
        let mut state = CertifiedState::new();
        for i in blocks {
            state.certify(block(i), parent(i), CertifiedStatus::Notarized);
        }
        state
    }

    /// The reports of the other validators this node can use: its own are
    /// usable by construction and are left out here so that the tests count
    /// what a reconciliation achieved
    fn theirs_usable(
        exchange: &mut ReportExchange,
        epoch: ConsensusEpoch,
    ) -> Vec<(BlockHash, BlockHash)> {
        let own: Vec<PublicKey> = exchange
            .epochs
            .get(&epoch)
            .map(|held| held.signed.iter().map(|report| report.reporter).collect())
            .unwrap_or_default();
        // The exchange tests exercise reconstruction; every certificate is
        // taken as assembled, and the predecessor as the empty genesis
        let reporters: Vec<PublicKey> = exchange
            .epochs
            .get(&epoch)
            .map(|held| held.theirs.keys().copied().collect())
            .unwrap_or_default();
        if let Some(held) = exchange.epochs.get_mut(&epoch) {
            held.predecessor.get_or_insert_with(|| {
                let genesis = crate::consensus::election::EpochLedger::new();
                (genesis.state_hash(), std::sync::Arc::new(genesis))
            });
        }
        for reporter in reporters {
            exchange.verify_evidence(epoch, &reporter, &AllCertified, later());
        }
        exchange
            .usable(epoch)
            .into_iter()
            .filter(|(report, _, _)| !own.contains(&report.reporter))
            .map(|(_, certified, residual)| (certified.root(), residual.root()))
            .collect()
    }

    /// Every certificate assembled: the exchange tests are about
    /// reconstruction, not about the votes behind the tags
    struct AllCertified;

    impl CertificateSource for AllCertified {
        fn kinds(
            &self,
            _: ConsensusEpoch,
            _: &BlockHash,
        ) -> crate::consensus::election::CertificateKinds {
            crate::consensus::election::CertificateKinds {
                nc: true,
                fc: true,
                ff: true,
            }
        }
    }

    /// A time later than any handed out before: every call to the exchange
    /// happens after the retry interval of the one before it
    fn later() -> Timestamp {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CALLS: AtomicU64 = AtomicU64::new(0);
        let calls = CALLS.fetch_add(1, Ordering::Relaxed);
        Timestamp::new_test_instance() + ReportExchange::SKETCH_INTERVAL * (calls as u32 + 1)
    }

    /// The one reconciliation request the exchange asks for now
    fn request_of(
        exchange: &mut ReportExchange,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
    ) -> ReconReq {
        let (messages, _) = exchange.reconcile(epoch, reporter, later()).unwrap();
        messages
            .into_iter()
            .find_map(|message| match message {
                ReportMessage::Request(request) => Some(request),
                _ => None,
            })
            .expect("expected a request")
    }

    fn signed(key: &PrivateKey, epoch: ConsensusEpoch, state: &CertifiedState) -> Report {
        signed_with(key, epoch, state, &ResidualVotes::new())
    }

    fn signed_with(
        key: &PrivateKey,
        epoch: ConsensusEpoch,
        state: &CertifiedState,
        residual: &ResidualVotes,
    ) -> Report {
        let payload = ReportCommitment {
            epoch,
            committee: BlockHash::from(7),
            predecessor: BlockHash::ZERO,
            certified: state.root(),
            residual: residual.root(),
            reporter: key.public_key(),
        }
        .payload();
        Report::new(
            key,
            epoch,
            BlockHash::from(7),
            BlockHash::ZERO,
            state.root(),
            residual.root(),
            payload,
        )
    }
}
