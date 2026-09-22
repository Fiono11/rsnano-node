mod epoch_decision;
mod report_plugin;
mod report_service;

pub use epoch_decision::EpochDecisionService;
pub(crate) use report_plugin::{ReportPlugin, ReportTicker};
pub use report_service::ReportService;

use std::collections::{BTreeMap, HashMap};

use rsnano_messages::{
    CertifiedEntry, ReconReply, ReconReq, Report, ResidualEntry, ResidualReply, ResidualReq,
};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey, PublicKey};

use crate::consensus::election::{
    Certification, CertifiedBlock, CertifiedState, CertifiedStatus, ReportCommitment, ResidualKind,
    ResidualVotes,
};

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
    /// The residual votes this node signed for
    residual: ResidualVotes,
    /// The residual objects this node can serve, by their roots: its own,
    /// and the ones it reconstructed from other reporters. A residual object
    /// never changes after its report is signed, so a root identifies it for
    /// good.
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
    /// The state reconstructed for the signed root, once a difference has
    /// rebuilt it
    reconstructed: Option<CertifiedState>,
    /// RAI: the residual object `g_i` commits to, once fetched and checked
    /// against the signed root. A report is usable only with both.
    residual: Option<ResidualVotes>,
    /// The residual object being fetched, in canonical order
    residual_partial: Option<ResidualVotes>,
}

impl TheirReport {
    /// RAI: a report is usable only after reconstructing `T_i` and `G_i` and
    /// recomputing both signed roots. A certified state without the
    /// reporter's residual votes would lose every candidate that has no
    /// certificate, which is what `Include_Q` and `A_Q` rest on.
    fn is_complete(&self) -> bool {
        self.reconstructed.is_some() && self.residual.is_some()
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
    /// Ask for the residual object a report committed to
    ResidualRequest(ResidualReq),
    /// Answer a residual fetch with the part of the object asked for
    ResidualAnswer(ResidualReply),
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
        held.residuals.insert(residual_root, residual.clone());
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
    /// votes behind a certificate. What is delivered is merged in, never
    /// replaced: a certificate constructed in an instance since erased is
    /// still a historical record of the epoch. The state a report signed
    /// stays in the history, so this node can still bridge to it, and so
    /// does the state it was asked to bridge from: a requester which
    /// advertised a root has to be answerable once this node reaches it.
    pub fn refresh_live(&mut self, epoch: ConsensusEpoch, live: CertifiedState) {
        let held = self.epochs.entry(epoch).or_default();
        let before = held.live.clone();
        for (block, entry) in live.entries() {
            held.live.certify(*block, entry.previous, entry.status);
        }
        if held.live.root() == before.root() {
            return;
        }
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
            if own.certified != certified || own.residual != residual {
                return None;
            }
            return Some((held.state(certified)?, held.residuals.get(&residual)?));
        }
        let their = held.theirs.get(reporter)?;
        if their.report.certified != certified || their.report.residual != residual {
            return None;
        }
        Some((their.reconstructed.as_ref()?, their.residual.as_ref()?))
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
        let theirs = held.theirs.values().filter_map(|their| {
            Some((
                &their.report,
                their.reconstructed.as_ref()?,
                their.residual.as_ref()?,
            ))
        });
        own.chain(theirs).collect()
    }

    /// RAI: a report is taken only with a valid signature over its epoch,
    /// committee, both roots and the reporter. It is stored, not
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
        // An empty residual object is settled by its signed root alone:
        // every replica knows the root of the empty object
        let residual = (report.residual == ResidualVotes::new().root()).then(ResidualVotes::new);
        held.theirs.insert(
            report.reporter,
            TheirReport {
                report,
                asked: None,
                reconstructed: None,
                residual,
                residual_partial: None,
            },
        );
        true
    }

    /// RAI: make a report usable. The certified state is reconstructed from
    /// a difference: a state this node already holds that hashes to the
    /// signed root needs no message at all, which is the common case between
    /// validators that saw the same evidence; otherwise the request names
    /// the roots this node holds and a replica that knows one of them and the
    /// target answers. The residual object is fetched whole, and is often
    /// empty, in which case the signed root settles it without a message.
    ///
    /// A request that goes unanswered is repeated after `RETRY_INTERVAL`,
    /// with the live root as it stands then. That is the whole of the retry:
    /// the live states of correct validators converge as the epoch's
    /// evidence arrives, and a source shared with a correct reporter is
    /// found then.
    pub fn reconcile(
        &mut self,
        epoch: ConsensusEpoch,
        reporter: PublicKey,
        now: Timestamp,
    ) -> Option<(Vec<ReportMessage>, Option<ReconcileResult>)> {
        let held = self.epochs.get_mut(&epoch)?;
        let own_residual = held.residuals.get(&held.residual.root()).cloned();
        let (target, residual_target, needs_state) = {
            let their = held.theirs.get(&reporter)?;
            if their.is_complete() {
                return None;
            }
            (
                their.report.certified,
                their.report.residual,
                their.reconstructed.is_none(),
            )
        };
        // A state already held that hashes to the target: nothing to ask for
        let known = if needs_state {
            held.state(target).cloned()
        } else {
            None
        };
        let sources = held.shared_sources();
        let their = held.theirs.get_mut(&reporter)?;
        let mut messages = Vec::new();
        let mut result = None;
        let may_ask = their
            .asked
            .is_none_or(|asked| asked.elapsed(now) >= Self::RETRY_INTERVAL);

        if their.residual.is_none() {
            if own_residual
                .as_ref()
                .is_some_and(|own| own.root() == residual_target)
            {
                // This node's own residual object hashes to the same root:
                // the two reporters recorded the same votes
                their.residual = own_residual;
            } else if may_ask {
                let offset = their
                    .residual_partial
                    .as_ref()
                    .map(ResidualVotes::len)
                    .unwrap_or(0);
                messages.push(ReportMessage::ResidualRequest(ResidualReq {
                    epoch,
                    root: residual_target,
                    offset: offset as u32,
                }));
            }
        }

        if needs_state {
            if let Some(state) = known {
                let total = state.len();
                their.reconstructed = Some(state);
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
            }
        }
        if !messages.is_empty() {
            their.asked = Some(now);
        }
        Some((messages, result))
    }

    /// RAI: answer a request if this node knows the target and one of the
    /// sources offered. Either may be its live state, one it retained or a
    /// report it reconstructed; a replica that knows only one side does not
    /// answer, and no answer is not a verdict. Of the sources it knows, it
    /// takes the one closest to the target.
    ///
    /// The difference is the evidence two states of one epoch differ in and
    /// is expected to be small. One that does not fit a reply goes
    /// unanswered: the requester's live state keeps converging, and a later
    /// shared source is closer to the target.
    pub fn handle_request(&self, request: &ReconReq) -> Result<ReconReply, ReconRefusal> {
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
        if delta.len() > ReconReply::MAX_ENTRIES {
            return Err(ReconRefusal::TooLarge(delta.len()));
        }
        let entry = |block: &CertifiedBlock, held: &Certification| CertifiedEntry {
            account: block.account,
            height: block.height,
            hash: block.hash,
            previous: held.previous,
            status: held.status.as_byte(),
        };
        Ok(ReconReply {
            epoch: request.epoch,
            source: source_root,
            target: request.target,
            added: delta
                .added
                .iter()
                .map(|(block, held)| entry(block, held))
                .collect(),
            removed: delta
                .removed
                .iter()
                .filter_map(|block| Some(entry(block, &source.certification(block)?)))
                .collect(),
        })
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
        let mut state = held.state(reply.source)?.clone();
        for entry in &reply.removed {
            state.remove(&CertifiedBlock::new(
                entry.account,
                entry.height,
                entry.hash,
            ));
        }
        for entry in &reply.added {
            let Some(status) = CertifiedStatus::from_byte(entry.status) else {
                continue;
            };
            state.set(
                CertifiedBlock::new(entry.account, entry.height, entry.hash),
                Certification {
                    status,
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

    /// RAI: serve the residual object a report committed to. It never
    /// changes, so it is fetched whole rather than differenced; anyone that
    /// has reconstructed it serves it, not only its reporter.
    pub fn handle_residual_request(&self, request: &ResidualReq) -> Option<ResidualReply> {
        let held = self.epochs.get(&request.epoch)?;
        let object = held.residuals.get(&request.root)?;
        let entries = object
            .entries()
            .skip(request.offset as usize)
            .take(ResidualReply::MAX_ENTRIES)
            .map(|(block, kind, previous)| ResidualEntry {
                account: block.account,
                height: block.height,
                hash: block.hash,
                previous,
                kind: kind.as_byte(),
            })
            .collect();
        Some(ResidualReply {
            epoch: request.epoch,
            root: request.root,
            offset: request.offset,
            entries,
        })
    }

    /// RAI: accumulate a residual object and accept it exactly when what has
    /// been accumulated hashes to the root its reporter signed. The fetch is
    /// gossiped and answered by several replicas, so the parts arrive in any
    /// order: only the part that continues what is held is taken, in
    /// canonical order, and the others are asked for again.
    pub fn handle_residual_reply(&mut self, reply: &ResidualReply) -> Option<ReconcileResult> {
        let held = self.epochs.get_mut(&reply.epoch)?;
        let reporter = held
            .theirs
            .iter()
            .find(|(_, their)| their.report.residual == reply.root && their.residual.is_none())
            .map(|(reporter, _)| *reporter)?;
        let their = held.theirs.get_mut(&reporter)?;
        let mut object = their.residual_partial.clone().unwrap_or_default();
        if reply.offset as usize != object.len() {
            return None;
        }
        let mut last = object
            .entries()
            .next_back()
            .map(|(block, kind, _)| (block, kind));
        for entry in &reply.entries {
            let block = CertifiedBlock::new(entry.account, entry.height, entry.hash);
            let Some(kind) = ResidualKind::from_byte(entry.kind) else {
                continue;
            };
            if last.is_some_and(|held| (block, kind) <= held) {
                continue;
            }
            last = Some((block, kind));
            object.record(block, entry.previous, kind);
        }
        let complete = object.root() == reply.root;
        let total = object.len();
        if complete {
            their.residual_partial = None;
            their.residual = Some(object.clone());
            held.residuals.insert(reply.root, object);
        } else {
            their.residual_partial = (!reply.entries.is_empty()).then_some(object);
        }
        let complete_report = held.theirs.get(&reporter)?.is_complete();
        Some(ReconcileResult {
            epoch: reply.epoch,
            reporter,
            complete: complete_report,
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

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::Account;
    use std::time::Duration;

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
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &theirs)));

        let (messages, result) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        assert!(messages.is_empty(), "nothing to ask for");
        let result = result.expect("complete at once");
        assert!(result.complete);
        assert_eq!(result.entries, 0);
        assert_eq!(theirs_usable(&ours, epoch).len(), 1);
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
        let usable = theirs_usable(&ours, epoch);
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
            theirs_usable(&ours, epoch),
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
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed(&key, epoch, &state_of(0..10))));
        let request = request_of(&mut ours, epoch, key.public_key());

        // One entry short of the target
        let reply = ReconReply {
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
        assert!(theirs_usable(&ours, epoch).is_empty());
        // The request is repeated until an answer reaches the root
        assert!(
            !request_of(&mut ours, epoch, key.public_key())
                .sources
                .is_empty()
        );
    }

    /// A difference that does not fit one reply is not answered: the
    /// requester's live state keeps converging, and a later shared source is
    /// closer to the target
    #[test]
    fn a_difference_larger_than_a_reply_is_not_answered() {
        let epoch = ConsensusEpoch::ZERO;
        // Three entries short of the target, then one too many to fit
        let far = ReconReply::MAX_ENTRIES as u64 + 4;
        let mut exchange = ReportExchange::new();
        exchange.report_epoch(
            epoch,
            state_of(0..3),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(1)],
        );
        let near = exchange.live_root(epoch).unwrap();
        exchange.refresh_live(epoch, state_of(0..far));
        let request = ReconReq {
            epoch,
            target: exchange.live_root(epoch).unwrap(),
            sources: vec![near],
        };
        assert_eq!(
            exchange.handle_request(&request),
            Err(ReconRefusal::TooLarge(ReconReply::MAX_ENTRIES + 1))
        );
        // Once closer, it is
        exchange.refresh_live(epoch, state_of(0..(far + 1)));
        let request = ReconReq {
            sources: vec![state_of(0..far).root()],
            target: exchange.live_root(epoch).unwrap(),
            ..request
        };
        assert_eq!(exchange.handle_request(&request).unwrap().added.len(), 1);
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
        assert_eq!(theirs_usable(&ours, epoch).len(), 2);
    }

    /// RAI: "A report is usable only after reconstructing T_i and G_i". The
    /// certified state alone does not make it usable; the residual object is
    /// fetched whole, because it holds the reporter's own votes and no state
    /// of the requester's can be differenced against it.
    #[test]
    fn a_report_is_usable_only_once_its_residual_object_is_fetched_too() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let theirs = state_of(0..10);
        let mut residual = ResidualVotes::new();
        for i in 20..24 {
            residual.record(block(i), parent(i), ResidualKind::First);
        }

        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            epoch,
            theirs.clone(),
            residual.clone(),
            BlockHash::from(7),
            &[key.clone()],
        );

        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            theirs.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed_with(&key, epoch, &theirs, &residual)));

        // The certified state matches at once, but the report is not usable
        let (messages, result) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        assert!(!result.expect("the state matched").complete);
        assert!(theirs_usable(&ours, epoch).is_empty());

        let Some(ReportMessage::ResidualRequest(request)) = messages
            .into_iter()
            .find(|m| matches!(m, ReportMessage::ResidualRequest(_)))
        else {
            panic!("expected a residual fetch");
        };
        let reply = reporter
            .handle_residual_request(&request)
            .expect("the reporter serves its own object");
        assert_eq!(reply.entries.len(), 4);
        assert!(ours.handle_residual_reply(&reply).unwrap().complete);

        let usable = theirs_usable(&ours, epoch);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].1, residual.root());

        // And a reconstructor serves the object in turn, so an unavailable
        // reporter does not make it unfetchable
        let second = ours
            .handle_residual_request(&ResidualReq {
                epoch,
                root: residual.root(),
                offset: 0,
            })
            .expect("a reconstructor serves it too");
        assert_eq!(second.entries.len(), 4);
    }

    /// A residual object that is empty needs no fetch at all: its signed root
    /// is the root of the empty object, which every replica knows
    #[test]
    fn an_empty_residual_object_needs_no_fetch() {
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

        let (messages, result) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        assert!(messages.is_empty());
        assert!(result.unwrap().complete);
        assert_eq!(theirs_usable(&ours, epoch).len(), 1);
    }

    /// A residual object larger than one reply is fetched in canonical
    /// chunks, each continuing where the last left off
    #[test]
    fn a_large_residual_object_is_fetched_in_chunks() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let theirs = state_of(0..4);
        let mut residual = ResidualVotes::new();
        for i in 0..(ResidualReply::MAX_ENTRIES as u64 + 30) {
            residual.record(block(1000 + i), parent(1000 + i), ResidualKind::First);
        }
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            epoch,
            theirs.clone(),
            residual.clone(),
            BlockHash::from(7),
            &[key.clone()],
        );
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            theirs.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed_with(&key, epoch, &theirs, &residual)));

        let mut replies = 0;
        loop {
            let (messages, _) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
            let Some(ReportMessage::ResidualRequest(request)) = messages
                .into_iter()
                .find(|m| matches!(m, ReportMessage::ResidualRequest(_)))
            else {
                panic!("expected a residual fetch");
            };
            let reply = reporter.handle_residual_request(&request).unwrap();
            assert!(reply.entries.len() <= ResidualReply::MAX_ENTRIES);
            replies += 1;
            assert!(replies < 10, "the fetch has to terminate");
            if ours.handle_residual_reply(&reply).unwrap().complete {
                break;
            }
        }
        assert_eq!(replies, 2);
        assert_eq!(theirs_usable(&ours, epoch).len(), 1);
    }

    /// The parts of a residual object arrive in any order, from different
    /// replicas: a part that does not continue what is held is not applied,
    /// and the fetch goes on from where it stands
    #[test]
    fn a_residual_part_out_of_order_is_not_applied() {
        let epoch = ConsensusEpoch::ZERO;
        let key = PrivateKey::from(1);
        let theirs = state_of(0..4);
        let mut residual = ResidualVotes::new();
        for i in 0..(ResidualReply::MAX_ENTRIES as u64 + 30) {
            residual.record(block(1000 + i), parent(1000 + i), ResidualKind::First);
        }
        let mut reporter = ReportExchange::new();
        reporter.report_epoch(
            epoch,
            theirs.clone(),
            residual.clone(),
            BlockHash::from(7),
            &[key.clone()],
        );
        let mut ours = ReportExchange::new();
        ours.report_epoch(
            epoch,
            theirs.clone(),
            ResidualVotes::new(),
            BlockHash::from(7),
            &[PrivateKey::from(2)],
        );
        assert!(ours.handle_report(signed_with(&key, epoch, &theirs, &residual)));
        let (messages, _) = ours.reconcile(epoch, key.public_key(), later()).unwrap();
        let Some(ReportMessage::ResidualRequest(first)) = messages
            .into_iter()
            .find(|m| matches!(m, ReportMessage::ResidualRequest(_)))
        else {
            panic!("expected a residual fetch");
        };
        assert_eq!(first.offset, 0);
        let second = ResidualReq {
            offset: ResidualReply::MAX_ENTRIES as u32,
            ..first.clone()
        };
        // The second part arrives first: ignored
        let tail = reporter.handle_residual_request(&second).unwrap();
        assert_eq!(tail.entries.len(), 30);
        assert!(ours.handle_residual_reply(&tail).is_none());
        // Then the first, then the second again: complete
        let head = reporter.handle_residual_request(&first).unwrap();
        assert!(!ours.handle_residual_reply(&head).unwrap().complete);
        assert!(ours.handle_residual_reply(&tail).unwrap().complete);
        assert_eq!(theirs_usable(&ours, epoch).len(), 1);
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
            &[key.clone()],
        );
        // The live state moves on; the report's own snapshot is still usable
        exchange.refresh_live(epoch, state_of(0..14));

        let usable = exchange.usable(epoch);
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].0.reporter, key.public_key());
        assert_eq!(usable[0].1.root(), certified.root());
        assert_eq!(usable[0].2.root(), residual.root());

        let named = exchange
            .usable_report(epoch, &key.public_key(), certified.root(), residual.root())
            .expect("a proposal may name it");
        assert_eq!(named.0.root(), certified.root());
        assert_eq!(named.1.first_votes().count(), 1);

        // A proposal naming the wrong roots for this reporter is refused
        assert!(
            exchange
                .usable_report(
                    epoch,
                    &key.public_key(),
                    BlockHash::from(9),
                    residual.root()
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

    /// The live state only ever grows: a certificate the active elections no
    /// longer hold stays a record of the epoch, so a refresh merges rather
    /// than replaces
    #[test]
    fn the_live_state_is_monotone() {
        let epoch = ConsensusEpoch::ZERO;
        let mut exchange = ReportExchange::new();
        exchange.refresh_live(epoch, state_of(0..10));
        exchange.refresh_live(epoch, state_of(5..12));
        let mut expected = state_of(0..12);
        assert_eq!(exchange.live_root(epoch), Some(expected.root()));
        // A status upgrade is taken, a downgrade is not
        let mut upgraded = CertifiedState::new();
        upgraded.certify(block(3), parent(3), CertifiedStatus::Finalized);
        exchange.refresh_live(epoch, upgraded);
        expected.certify(block(3), parent(3), CertifiedStatus::Finalized);
        assert_eq!(exchange.live_root(epoch), Some(expected.root()));
        exchange.refresh_live(epoch, state_of(3..4));
        assert_eq!(exchange.live_root(epoch), Some(expected.root()));
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
        exchange: &ReportExchange,
        epoch: ConsensusEpoch,
    ) -> Vec<(BlockHash, BlockHash)> {
        let own: Vec<PublicKey> = exchange
            .epochs
            .get(&epoch)
            .map(|held| held.signed.iter().map(|report| report.reporter).collect())
            .unwrap_or_default();
        exchange
            .usable(epoch)
            .into_iter()
            .filter(|(report, _, _)| !own.contains(&report.reporter))
            .map(|(_, certified, residual)| (certified.root(), residual.root()))
            .collect()
    }

    /// A time later than any handed out before: every call to the exchange
    /// happens after the retry interval of the one before it
    fn later() -> Timestamp {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CALLS: AtomicU64 = AtomicU64::new(0);
        let calls = CALLS.fetch_add(1, Ordering::Relaxed);
        Timestamp::new_test_instance() + ReportExchange::RETRY_INTERVAL * (calls as u32 + 1)
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
