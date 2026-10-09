use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rsnano_messages::{
    CheckpointReply, CheckpointReq, EpochProp, EvidenceReq, ManifestReply, ManifestReq, Message,
    ReportSelection,
};
use rsnano_network::{Channel, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{Amount, BlockHash, ConsensusEpoch, PublicKey};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use super::{
    ReportExchange,
    checkpoint_fetch::{CheckpointFetch, checkpoint_bytes},
    chunk_window::ChunkWindow,
    manifest_claims,
    report_service::inherited_from,
    unjustified,
};
use crate::{
    consensus::{
        AecService,
        active_elections::EpochProposalContext,
        election::{
            AccountSlot, BlockIndex, BuildRules, Committee, EpochLedger, EpochValue, Manifest,
            ManifestAssembly, MemberOrder, ReportIndex, ReportRef, ReportSource, SelectedReport,
            fresh_finalized, overlap_certified, overlap_claims, witness_claims,
        },
    },
    transport::MessageFlooder,
    utils::diagnostic,
    wallets::WalletRepresentatives,
};

/// What checking a proposal came to
enum Validation {
    /// The value derives the state proposed: this node can vote for it
    Accepted(BlockHash, Arc<EpochLedger>),
    /// Its reports, manifest or evidence are still being fetched; the
    /// leader's repeat brings it back here
    Pending,
    /// It can not be voted for: its evidence does not justify its claims or
    /// its state is not the one derived
    Refused,
    /// Checked already, or its epoch is decided
    Skipped,
}

/// Whether the evidence a value's manifest names is held and checked here
enum Evidence {
    Committed,
    Pending,
    Refused,
}

/// Chunks of a manifest asked for at once
const MANIFEST_WINDOW: usize = 8;

/// A manifest being fetched by digest: the chunks received and the ones
/// asked for
struct ManifestFetch {
    assembly: ManifestAssembly,
    window: ChunkWindow,
}

impl ManifestFetch {
    fn new(digest: BlockHash) -> Self {
        Self {
            assembly: ManifestAssembly::new(digest),
            window: ChunkWindow::new(MANIFEST_WINDOW),
        }
    }

    /// The chunk starts to ask for now
    fn due(&mut self, now: Timestamp, retry: std::time::Duration) -> Vec<u32> {
        let missing = self.assembly.missing_chunks(ManifestReply::MAX_ENTRIES);
        self.window.due(missing, now, retry)
    }
}

/// RAI, "Immutable candidate inputs": what a candidate's manifest must name
/// for a selection: the votes behind the selected reports' claims, the
/// exclusion witnesses that discharge the inherited recovery records those
/// claims bypass, and those that discharge the records of the overlap
/// anchor `S_{e-2}` a fresh finalized block bypasses (its overlap
/// certificate)
fn candidate_claims(
    epoch: ConsensusEpoch,
    states: &[SelectedReport],
    previous: &EpochLedger,
    anchor: Option<&EpochLedger>,
    index: &dyn BlockIndex,
) -> Vec<(ConsensusEpoch, BlockHash)> {
    let mut claims = manifest_claims(epoch, states, &|block, entry| {
        inherited_from(previous, block, entry)
    });
    claims.extend(witness_claims(previous, states, index));
    if let Some(anchor) = anchor {
        claims.extend(overlap_claims(anchor, &fresh_finalized(states, previous)));
    }
    claims.sort();
    claims.dedup();
    claims
}

/// RAI, "Commit admission witnesses": the manifest with the closing-epoch
/// witness of every overlap certificate its evidence proves marked. The
/// marks enter the digest, so the decided value fixes which fresh
/// finalized blocks the evidence admits under the overlap exception, and
/// with them the anchor and witnesses those admissions rest on. They are a
/// function of the evidence, the selection and the closed states: a
/// validator recomputes them, and a manifest marking any other set is
/// refused.
fn marked_manifest(
    evidence: &Manifest,
    epoch: ConsensusEpoch,
    states: &[SelectedReport],
    previous: &EpochLedger,
    anchor: Option<&EpochLedger>,
    members: &EpochMembers,
) -> Manifest {
    let mut marked = evidence.without_overlaps();
    let (Some(anchor), Some(before)) = (
        anchor,
        epoch.as_u64().checked_sub(1).map(ConsensusEpoch::new),
    ) else {
        return marked;
    };
    let fresh = fresh_finalized(states, previous);
    let certified = overlap_certified(
        &fresh,
        previous,
        anchor,
        &|hash| manifest_witness(evidence, members, before, hash),
        &|hash| {
            members.get(epoch).is_some_and(|(committee, order)| {
                evidence.kinds(epoch, hash, &committee, &order).notarization
            })
        },
        &|origin, hash| manifest_witness(evidence, members, origin, hash),
    );
    for hash in &certified {
        marked.set_overlap(before, hash);
    }
    marked
}

/// RAI, "The joint epoch election": the part of the close that reads and
/// writes. The election itself is in the active elections - it is Kudzu over
/// election slots, with the same vote pools and thresholds as an account
/// domain - and what happens here is the value: selecting `N - f` usable
/// reports, deriving `BuildState(S_{e-1}, Q_e)` from them, proposing
/// `X = (h_p, Q_e, d_e)`, and re-deriving that state to check a proposal
/// another leader made.
///
/// A validator never proposes a state of its own and never compares a
/// proposal with one. It checks that the reports named determine the state
/// named, which is a public function of evidence every replica can obtain.
pub struct EpochDecisionService {
    exchange: Arc<Mutex<ReportExchange>>,
    active_elections: Arc<AecService>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    flooder: Mutex<MessageFlooder>,
    clock: Arc<SteadyClock>,
    stats: Arc<Stats>,
    /// The proposals this node made, re-broadcast while their epoch is
    /// still closing: a replica that missed the message holds no value to
    /// vote for, and the round would time out for want of a message rather
    /// than of agreement.
    proposals: Mutex<HashMap<(ConsensusEpoch, u32), EpochProp>>,
    repeated: Mutex<Option<Timestamp>>,
    /// When this node last said why it could not derive a value
    unready_logged: Mutex<HashMap<ConsensusEpoch, Timestamp>>,
    /// RAI, "Immutable candidate inputs": the evidence manifests this node
    /// built or fetched, by digest, served to validators that commit to a
    /// proposal's evidence rather than to their own
    manifests: Mutex<HashMap<BlockHash, (ConsensusEpoch, Arc<Manifest>)>>,
    /// Manifests being fetched by digest
    assemblies: Mutex<HashMap<BlockHash, ManifestFetch>>,
    /// Since when each epoch has been closed here without its value derived
    closed_undecided: Mutex<HashMap<ConsensusEpoch, Timestamp>>,
    /// Decided states being fetched
    checkpoint_fetches: Mutex<HashMap<ConsensusEpoch, CheckpointFetch>>,
    /// The encoding of the state this node served last: a fetch asks for
    /// many chunks of the same state
    served_checkpoint: Mutex<Option<(ConsensusEpoch, BlockHash, Arc<Vec<u8>>)>>,
}

impl EpochDecisionService {
    /// How often this node repeats the proposals of its current rounds
    const REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
    /// How often a close that can not start says what it is short of
    const UNREADY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
    /// Epochs whose proposals this node keeps repeating: the recent ones, so
    /// that a straggler can still obtain a value while an old epoch's
    /// proposals are not repeated for the rest of the run
    const REPEATED_EPOCHS: usize = 4;

    pub(crate) fn new(
        exchange: Arc<Mutex<ReportExchange>>,
        active_elections: Arc<AecService>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            exchange,
            active_elections,
            wallet_reps,
            flooder: Mutex::new(flooder),
            clock,
            stats,
            proposals: Mutex::new(HashMap::new()),
            repeated: Mutex::new(None),
            unready_logged: Mutex::new(HashMap::new()),
            manifests: Mutex::new(HashMap::new()),
            assemblies: Mutex::new(HashMap::new()),
            closed_undecided: Mutex::new(HashMap::new()),
            checkpoint_fetches: Mutex::new(HashMap::new()),
            served_checkpoint: Mutex::new(None),
        }
    }

    /// How often a manifest chunk is asked for again
    const MANIFEST_RETRY: std::time::Duration = std::time::Duration::from_millis(300);

    /// RAI: drive the joint election of every epoch still closing. A replica
    /// takes part once it holds the decided predecessor state and enough
    /// usable reports to derive a value; the leader of the round it is in
    /// proposes one.
    pub fn tick(&self) {
        let closes = self.active_elections.epoch_closes();
        for close in &closes {
            // Not "until the epoch closes" but until it is decided here: a
            // replica that learned the certificate before it could derive
            // the value holds no state for the epoch, and it is the
            // derivation, not the certificate, that decides one. It goes on
            // collecting reports until it can.
            if close.value.is_some() {
                continue;
            }
            let ready = self.can_derive(close.epoch);
            if ready != close.ready {
                self.active_elections.set_close_ready(close.epoch, ready);
            }
        }
        for context in self.active_elections.epoch_proposals_due() {
            self.propose(context);
        }
        self.repeat_proposals();
        self.catch_up(&closes);
    }

    /// RAI: a leader's proposal. It is validated like any other: this node
    /// derives the state the named reports determine and requires its hash
    /// to be the one proposed. A value it can not check is one it does not
    /// vote for, and its round times out.
    pub fn handle_proposal(&self, prop: EpochProp, _channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::EpochProp, Direction::In);
        if prop.reports.is_empty() {
            return;
        }
        let value = EpochValue::from_parts(
            prop.epoch,
            prop.slot,
            prop.parent,
            prop.reports.iter().map(report_ref).collect(),
            prop.manifest,
            prop.state,
        );
        let hash = value.hash();
        if !prop.verify(hash) {
            return;
        }
        // A leader repeats its proposal while its round stands, and
        // deriving the state walks the whole predecessor: check once. Every
        // round's proposal of an epoch decided here is repeated to replicas
        // that hold the certificate but not the value; checking them here
        // changes nothing and costs a derivation each.
        if self.active_elections.holds_epoch_value(prop.epoch, &hash)
            || self.active_elections.epoch_decided(prop.epoch)
        {
            return;
        }
        let (epoch, slot) = (prop.epoch, prop.slot);
        match self.validate(&value, Some(_channel)) {
            Validation::Accepted(_, state) => {
                self.active_elections.accept_epoch_value(value, state);
            }
            // Still fetching or deriving: the round waits for the check
            Validation::Pending => {
                self.active_elections
                    .mark_epoch_proposal_checking(epoch, slot, hash);
            }
            Validation::Refused => {
                self.active_elections
                    .clear_epoch_proposal_checking(epoch, slot, &hash);
            }
            Validation::Skipped => {}
        }
    }

    /// Whether this node can derive a value for an epoch's close: it holds
    /// the decided predecessor state and reports carrying `N - f` of the old
    /// committee's weight
    fn can_derive(&self, epoch: ConsensusEpoch) -> bool {
        let Some(committee) = self.active_elections.epoch_committee(epoch) else {
            self.report_unready(epoch, "no committee", 0, Amount::ZERO, Amount::ZERO);
            return false;
        };
        let Some(previous) = self.active_elections.epoch_previous_state(epoch) else {
            self.report_unready(
                epoch,
                "no predecessor state",
                0,
                Amount::ZERO,
                committee.thresholds().report,
            );
            return false;
        };
        let predecessor = previous.state_hash();
        let (reporters, weight) = {
            let exchange = self.exchange.lock().unwrap();
            (
                exchange
                    .usable(epoch)
                    .iter()
                    .map(|(report, _, _)| report.reporter)
                    .collect::<Vec<_>>(),
                selected_weight(&exchange, epoch, &committee, predecessor),
            )
        };
        let usable = reporters.len();
        self.active_elections.set_close_reporters(epoch, reporters);
        if weight >= committee.thresholds().report {
            return true;
        }
        self.report_unready(
            epoch,
            "reports",
            usable,
            weight,
            committee.thresholds().report,
        );
        false
    }

    /// Why this node can not derive a value yet, at most once a second per
    /// epoch. A close that never starts is the one thing that stops an
    /// epoch for good, so it says what it is short of.
    fn report_unready(
        &self,
        epoch: ConsensusEpoch,
        reason: &str,
        usable: usize,
        weight: Amount,
        required: Amount,
    ) {
        let now = self.clock.now();
        {
            let mut last = self.unready_logged.lock().unwrap();
            if last
                .get(&epoch)
                .is_some_and(|last| last.elapsed(now) < Self::UNREADY_INTERVAL)
            {
                return;
            }
            last.insert(epoch, now);
        }
        let (held, roots) = {
            let exchange = self.exchange.lock().unwrap();
            let reports = exchange.reports(epoch);
            (
                reports.len(),
                reports
                    .iter()
                    .map(|report| {
                        format!(
                            "{}:{}:{}",
                            &report.reporter.to_string()[..8],
                            &report.certified.to_string()[..8],
                            &report.residual.to_string()[..8]
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };
        diagnostic!(
            "EPOCH_CLOSE_UNREADY epoch={} short_of={} usable={} weight={} required={} held={} theirs={:?}",
            epoch,
            reason,
            usable,
            weight.number(),
            required.number(),
            held,
            roots
        );
    }

    /// RAI, "Leader behavior": propose into the round this node leads. With
    /// a joint-complete parent it copies that parent's `(Q_e, d_e)`; with
    /// none it extends election genesis and selects `N - f` usable reports
    /// of its own.
    fn propose(&self, context: EpochProposalContext) {
        let epoch = context.epoch;
        let round = context.round;
        let built = match (context.parent.clone(), context.parent_state.clone()) {
            (Some(parent), Some(state)) => Some((parent.extend(round), state)),
            _ => self
                .reproposal(epoch, round)
                .or_else(|| self.derive_fresh(epoch, round)),
        };
        let Some((value, state)) = built else {
            return;
        };
        let hash = value.hash();
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        let Some(key) = keys
            .iter()
            .find(|key| key.public_key() == context.leader)
            .cloned()
        else {
            return;
        };
        diagnostic!(
            "EPOCH_PROPOSED epoch={} round={} value={} state={} reports={}",
            epoch,
            round,
            hash,
            value.state,
            value.reports().len()
        );
        let prop = EpochProp::new(
            &key,
            epoch,
            round,
            value.parent,
            value.manifest,
            value.state,
            value.reports().iter().map(selection_of).collect(),
            hash,
        );
        self.active_elections.accept_epoch_value(value, state);
        self.active_elections
            .record_epoch_proposal(epoch, round, hash);
        self.proposals
            .lock()
            .unwrap()
            .insert((epoch, round), prop.clone());
        self.broadcast(prop);
    }

    /// RAI, leader behaviour: a child of election genesis this node checked
    /// in an earlier round is proposed again, in place of a selection with a
    /// manifest of this leader's own. The replicas that checked it, in its
    /// round or after that round timed out, vote for it at once; a fresh
    /// manifest would send every validator fetching and deriving again, and
    /// a check slower than the round is what fails the rounds of a close.
    /// The payload is the same, only the slot differs; this node holds the
    /// manifest it names, so it can serve it.
    fn reproposal(
        &self,
        epoch: ConsensusEpoch,
        round: u32,
    ) -> Option<(EpochValue, Arc<EpochLedger>)> {
        let (held, state) = self.active_elections.validated_epoch_genesis_child(epoch)?;
        if held.slot == round {
            return None;
        }
        let value = EpochValue::from_parts(
            epoch,
            round,
            BlockHash::ZERO,
            held.reports().to_vec(),
            held.manifest,
            held.state,
        );
        diagnostic!(
            "EPOCH_REPROPOSED epoch={} round={} from_round={} manifest={}",
            epoch,
            round,
            held.slot,
            held.manifest
        );
        Some((value, state))
    }

    /// RAI: select the usable reports and derive the state they determine.
    /// The selection is the shortest prefix in canonical reporter order that
    /// carries `N - f` of the old committee's weight, so two correct leaders
    /// holding the same reports propose the same value and the first round
    /// decides.
    fn derive_fresh(
        &self,
        epoch: ConsensusEpoch,
        round: u32,
    ) -> Option<(EpochValue, Arc<EpochLedger>)> {
        let previous = self.active_elections.epoch_previous_state(epoch)?;
        let committee = self.active_elections.epoch_committee(epoch)?;
        let predecessor = previous.state_hash();
        let exchange = self.exchange.lock().unwrap();
        // Only reports signed against the same predecessor checkpoint, by
        // members of the committee they are counted in: a representative
        // outside it reports too, and its report has no place in Q_e
        let mut usable: Vec<PublicKey> = exchange
            .usable(epoch)
            .iter()
            .filter(|(report, _, _)| report.predecessor == predecessor)
            .map(|(report, _, _)| report.reporter)
            .filter(|reporter| !committee.weight(reporter).is_zero())
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
        let rules = BuildRules {
            many: committee.thresholds().many,
            epoch,
        };
        // The evidence this leader used, committed to by digest, with the
        // overlap certificates it proves marked
        let anchor = self.overlap_anchor(epoch)?;
        let started = std::time::Instant::now();
        let claims = candidate_claims(epoch, &states, &previous, anchor.as_deref(), &index);
        let claimed = started.elapsed();
        let members = EpochMembers::new(&self.active_elections);
        let manifest = Arc::new(marked_manifest(
            &self.active_elections.evidence_manifest(&claims),
            epoch,
            &states,
            &previous,
            anchor.as_deref(),
            &members,
        ));
        let digest = manifest.digest();
        let manifested = started.elapsed();
        self.remember_manifest(epoch, manifest.clone());
        let witness = |origin: ConsensusEpoch, hash: &BlockHash| {
            manifest_witness(&manifest, &members, origin, hash)
        };
        match EpochValue::propose(
            epoch,
            round,
            BlockHash::ZERO,
            &previous,
            &resolved,
            digest,
            &index,
            rules,
            &witness,
        ) {
            Ok((value, ledger)) => {
                diagnostic!(
                    "EPOCH_DERIVE_TIMING epoch={} claims={} overlaps={} claim_ms={} manifest_ms={} build_ms={}",
                    epoch,
                    claims.len(),
                    manifest.overlaps().count(),
                    claimed.as_millis(),
                    (manifested - claimed).as_millis(),
                    (started.elapsed() - manifested).as_millis()
                );
                Some((value, Arc::new(ledger)))
            }
            Err(error) => {
                diagnostic!(
                    "EPOCH_PROPOSAL_REFUSED epoch={} round={} reason={:?}",
                    epoch,
                    round,
                    error
                );
                None
            }
        }
    }

    /// RAI: derive the state a value's reports determine and check that it
    /// hashes to the `d_e` the value carries
    fn validate(&self, value: &EpochValue, channel: Option<&Arc<Channel>>) -> Validation {
        let Some(previous) = self.active_elections.epoch_previous_state(value.epoch) else {
            return Validation::Pending;
        };
        let Some(committee) = self.active_elections.epoch_committee(value.epoch) else {
            return Validation::Pending;
        };
        let exchange = self.exchange.lock().unwrap();
        // Copies of a proposal arrive from several peers and its leader
        // repeats it every `REPEAT_INTERVAL`, so several wait here while one
        // is checked, a second or more with a fetched manifest. Once that
        // check has accepted the value, or the epoch is decided, the waiting
        // copies return instead of checking it again while every report
        // message waits on the exchange.
        if self
            .active_elections
            .holds_epoch_value(value.epoch, &value.hash())
            || self.active_elections.epoch_decided(value.epoch)
        {
            return Validation::Skipped;
        }
        // The same payload checked in another slot decides the same state
        if let Some(state) = self.active_elections.epoch_state_for_payload(value) {
            diagnostic!(
                "EPOCH_VALUE_REUSED epoch={} slot={} value={}",
                value.epoch,
                value.slot,
                value.hash()
            );
            return Validation::Accepted(value.hash(), state);
        }
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
            return Validation::Pending;
        }
        let Some(anchor) = self.overlap_anchor(value.epoch) else {
            return Validation::Pending;
        };
        let started = std::time::Instant::now();
        match self.evidence_committed(value, &states, &previous, anchor.as_deref(), channel) {
            Evidence::Committed => {}
            Evidence::Pending => return Validation::Pending,
            Evidence::Refused => return Validation::Refused,
        }
        let committed = started.elapsed();
        // Held once the evidence is committed: built here or fetched
        let held = self
            .manifests
            .lock()
            .unwrap()
            .get(&value.manifest)
            .map(|(_, manifest)| manifest.clone());
        let Some(manifest) = held else {
            return Validation::Pending;
        };
        let members = EpochMembers::new(&self.active_elections);
        let witness = |origin: ConsensusEpoch, hash: &BlockHash| {
            manifest_witness(&manifest, &members, origin, hash)
        };
        let index = ReportIndex::new(&previous, &states);
        let rules = BuildRules {
            many: committee.thresholds().many,
            epoch: value.epoch,
        };
        match value.validate(
            &previous,
            &source,
            &index,
            committee.thresholds().report,
            rules,
            &witness,
        ) {
            Ok(ledger) => {
                diagnostic!(
                    "EPOCH_VALIDATE_TIMING epoch={} evidence_ms={} build_ms={}",
                    value.epoch,
                    committed.as_millis(),
                    (started.elapsed() - committed).as_millis()
                );
                Validation::Accepted(value.hash(), Arc::new(ledger))
            }
            Err(error) => {
                diagnostic!(
                    "EPOCH_VALUE_REFUSED epoch={} slot={} value={} reason={:?}",
                    value.epoch,
                    value.slot,
                    value.hash(),
                    error
                );
                Validation::Refused
            }
        }
    }

    /// Repeats the proposals of the rounds this node leads: a replica that
    /// missed the message holds no value to vote for, and the round would
    /// time out for want of a message rather than of agreement.
    /// RAI, "Immutable candidate inputs": whether this node holds and has
    /// checked the evidence the value's manifest names. A manifest equal to
    /// the one this node builds from its own votes is checked already, since
    /// those votes made the reports usable. Another manifest is fetched by
    /// digest, its votes fetched where lacking, and every claim of the
    /// selected reports is then checked against those votes alone: "A
    /// verifier must possess and validate the material addressed by M."
    fn evidence_committed(
        &self,
        value: &EpochValue,
        states: &[SelectedReport],
        previous: &EpochLedger,
        anchor: Option<&EpochLedger>,
        channel: Option<&Arc<Channel>>,
    ) -> Evidence {
        let epoch = value.epoch;
        let claims = candidate_claims(
            epoch,
            states,
            previous,
            anchor,
            &ReportIndex::new(previous, states),
        );
        let members = EpochMembers::new(&self.active_elections);
        let own = marked_manifest(
            &self.active_elections.evidence_manifest(&claims),
            epoch,
            states,
            previous,
            anchor,
            &members,
        );
        if own.digest() == value.manifest {
            self.remember_manifest(epoch, Arc::new(own));
            return Evidence::Committed;
        }
        let held = self
            .manifests
            .lock()
            .unwrap()
            .get(&value.manifest)
            .map(|(_, manifest)| manifest.clone());
        let Some(manifest) = held else {
            self.request_manifest(epoch, value.manifest, channel);
            return Evidence::Pending;
        };
        let missing = self.active_elections.missing_manifest_votes(&manifest);
        if !missing.is_empty() {
            let mut by_epoch: HashMap<ConsensusEpoch, Vec<BlockHash>> = HashMap::new();
            for (epoch, hash) in &missing {
                by_epoch.entry(*epoch).or_default().push(*hash);
            }
            diagnostic!(
                "EPOCH_MANIFEST_VOTES_MISSING epoch={} manifest={} hashes={}",
                epoch,
                value.manifest,
                missing.len()
            );
            for (epoch, hashes) in by_epoch {
                for chunk in hashes.chunks(EvidenceReq::MAX_HASHES) {
                    self.stats
                        .inc_dir(StatType::Message, DetailType::EvidenceReq, Direction::Out);
                    self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                        &Message::EvidenceReq(EvidenceReq {
                            epoch,
                            hashes: chunk.to_vec(),
                        }),
                        TrafficType::Generic,
                        1.0,
                    );
                }
            }
            return Evidence::Pending;
        }
        // The overlap marks: exactly the certificates the evidence proves
        let proven = marked_manifest(&manifest, epoch, states, previous, anchor, &members);
        if proven.digest() != value.manifest {
            diagnostic!(
                "EPOCH_MANIFEST_OVERLAPS_REFUSED epoch={} manifest={} marked={} proven={}",
                epoch,
                value.manifest,
                manifest.overlaps().count(),
                proven.overlaps().count()
            );
            return Evidence::Refused;
        }
        // Every claim, justified by the manifest's votes alone
        for report in states {
            let kinds = |kinds_epoch: ConsensusEpoch, hashes: &[BlockHash]| {
                let (committee, order) = members.get(kinds_epoch)?;
                Some(
                    hashes
                        .iter()
                        .map(|hash| manifest.kinds(kinds_epoch, hash, &committee, &order))
                        .collect(),
                )
            };
            let reporter_votes =
                |votes: &[(BlockHash, crate::consensus::election::ResidualKind)]| {
                    let order = members.get(epoch).map(|(_, order)| order);
                    votes
                        .iter()
                        .map(|(hash, kind)| {
                            order.as_ref().is_some_and(|order| {
                                manifest.names_vote(epoch, &report.reporter, hash, *kind, order)
                            })
                        })
                        .collect()
                };
            let justified = unjustified(
                epoch,
                report.certified,
                report.residual,
                None,
                &kinds,
                &|block, entry| inherited_from(previous, block, entry),
                &reporter_votes,
                &|block, entry| {
                    previous.admits(
                        AccountSlot::new(block.account, block.height),
                        block.hash,
                        entry.previous,
                        &|origin, hash| manifest_witness(&manifest, &members, origin, hash),
                    )
                },
            );
            match justified {
                Some(missing) if missing.is_empty() => {}
                _ => {
                    diagnostic!(
                        "EPOCH_MANIFEST_UNJUSTIFIED epoch={} manifest={} reporter={}",
                        epoch,
                        value.manifest,
                        report.reporter
                    );
                    return Evidence::Refused;
                }
            }
        }
        Evidence::Committed
    }

    /// RAI: the last closed state an overlap admission in the epoch was
    /// checked against, `S_{e-2}` (the genesis state for epoch 1); none for
    /// epoch 0, which follows no closing epoch. None outside: not held here
    fn overlap_anchor(&self, epoch: ConsensusEpoch) -> Option<Option<Arc<EpochLedger>>> {
        match epoch.as_u64().checked_sub(1) {
            None => Some(None),
            Some(before) => self
                .active_elections
                .epoch_previous_state(ConsensusEpoch::new(before))
                .map(Some),
        }
    }

    /// Keeps a manifest for serving, dropping those of old epochs
    fn remember_manifest(&self, epoch: ConsensusEpoch, manifest: Arc<Manifest>) {
        let mut manifests = self.manifests.lock().unwrap();
        manifests.insert(manifest.digest(), (epoch, manifest));
        if let Some(latest) = manifests.values().map(|(epoch, _)| *epoch).max() {
            manifests.retain(|_, (held, _)| {
                held.as_u64() + Self::REPEATED_EPOCHS as u64 > latest.as_u64()
            });
        }
    }

    /// Asks for the chunks of a manifest this node lacks, several at once
    /// and each at most once per retry interval: from the node the proposal
    /// came from, which holds it, or from every representative when that is
    /// unknown. Flooding every chunk request would bring back a reply per
    /// holder and swamp the inbound queues; asking for one chunk at a time
    /// made a manifest of tens of thousands of entries a long series of
    /// round trips, longer than a close round.
    fn request_manifest(
        &self,
        epoch: ConsensusEpoch,
        digest: BlockHash,
        channel: Option<&Arc<Channel>>,
    ) {
        let now = self.clock.now();
        let due = self
            .assemblies
            .lock()
            .unwrap()
            .entry(digest)
            .or_insert_with(|| ManifestFetch::new(digest))
            .due(now, Self::MANIFEST_RETRY);
        for from in due {
            self.send_manifest_request(
                ManifestReq {
                    epoch,
                    manifest: digest,
                    from,
                },
                channel,
            );
        }
    }

    fn send_manifest_request(&self, request: ManifestReq, channel: Option<&Arc<Channel>>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ManifestReq, Direction::Out);
        let mut flooder = self.flooder.lock().unwrap();
        match channel {
            Some(channel) => {
                let _ = flooder.try_send(
                    channel,
                    &Message::ManifestReq(request),
                    TrafficType::Generic,
                );
            }
            None => {
                flooder.flood_prs_and_some_non_prs(
                    &Message::ManifestReq(request),
                    TrafficType::Generic,
                    1.0,
                );
            }
        }
    }

    /// RAI: a validator asks for a manifest this node holds; one chunk goes back
    pub fn handle_manifest_request(&self, request: ManifestReq, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ManifestReq, Direction::In);
        let held = self
            .manifests
            .lock()
            .unwrap()
            .get(&request.manifest)
            .map(|(_, manifest)| manifest.clone());
        let Some(manifest) = held else {
            return;
        };
        let reply = ManifestReply {
            epoch: request.epoch,
            manifest: request.manifest,
            total: manifest.len() as u32,
            from: request.from,
            entries: manifest.chunk(request.from as usize, ManifestReply::MAX_ENTRIES),
        };
        self.stats
            .inc_dir(StatType::Message, DetailType::ManifestReply, Direction::Out);
        self.flooder.lock().unwrap().try_send(
            channel,
            &Message::ManifestReply(reply),
            TrafficType::Generic,
        );
    }

    /// RAI: a chunk of a manifest this node asked for. The complete manifest
    /// is checked against its digest and kept; the proposal that named it is
    /// checked again when its leader repeats it.
    pub fn handle_manifest_reply(&self, reply: ManifestReply, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::ManifestReply, Direction::In);
        let due = {
            let mut assemblies = self.assemblies.lock().unwrap();
            let Some(fetch) = assemblies.get_mut(&reply.manifest) else {
                return;
            };
            match fetch.assembly.take(reply.total, reply.from, &reply.entries) {
                Ok(Some(manifest)) => {
                    assemblies.remove(&reply.manifest);
                    diagnostic!(
                        "EPOCH_MANIFEST_FETCHED epoch={} manifest={} entries={}",
                        reply.epoch,
                        reply.manifest,
                        manifest.len()
                    );
                    self.remember_manifest(reply.epoch, Arc::new(manifest));
                    return;
                }
                Ok(None) => {
                    fetch.window.received(reply.from);
                    fetch.due(self.clock.now(), Self::MANIFEST_RETRY)
                }
                Err(()) => {
                    assemblies.remove(&reply.manifest);
                    diagnostic!(
                        "EPOCH_MANIFEST_REJECTED epoch={} manifest={} total={} from={}",
                        reply.epoch,
                        reply.manifest,
                        reply.total,
                        reply.from
                    );
                    return;
                }
            }
        };
        // The next chunks from the node that served this one
        for from in due {
            self.send_manifest_request(
                ManifestReq {
                    epoch: reply.epoch,
                    manifest: reply.manifest,
                    from,
                },
                Some(channel),
            );
        }
    }

    /// How long an epoch stays closed here without its value derived before
    /// its decided state is fetched instead: a derivation in progress
    /// usually finishes well within it
    const CATCH_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(1);
    /// How often a chunk of a decided state is asked for again
    const CHECKPOINT_RETRY: std::time::Duration = std::time::Duration::from_millis(500);

    /// RAI, checkpoint catch-up: an epoch whose close certificate this node
    /// holds, but whose value it could not derive - it lagged behind, and
    /// the reports the derivation needs were released once `N - f` of the
    /// successors had installed the checkpoint - takes the value and the
    /// decided state from a replica that holds them. The certificate binds
    /// the value's hash, and the value binds `d_e`, which the state fetched
    /// must hash to; neither the proposal nor the reports are needed.
    fn catch_up(&self, closes: &[crate::consensus::active_elections::EpochCloseInfo]) {
        let now = self.clock.now();
        let undecided: HashMap<ConsensusEpoch, BlockHash> = closes
            .iter()
            .filter(|close| close.value.is_none())
            .filter_map(|close| close.closed.map(|(_, value)| (close.epoch, value)))
            .collect();
        self.closed_undecided
            .lock()
            .unwrap()
            .retain(|epoch, _| undecided.contains_key(epoch));
        self.checkpoint_fetches
            .lock()
            .unwrap()
            .retain(|epoch, _| undecided.contains_key(epoch));
        for (epoch, closed) in undecided {
            let since = *self
                .closed_undecided
                .lock()
                .unwrap()
                .entry(epoch)
                .or_insert(now);
            if since.elapsed(now) < Self::CATCH_UP_AFTER {
                continue;
            }
            let due = {
                let mut fetches = self.checkpoint_fetches.lock().unwrap();
                let fetch = fetches.entry(epoch).or_insert_with(|| {
                    diagnostic!("EPOCH_CATCH_UP epoch={} value={}", epoch, closed);
                    CheckpointFetch::new(epoch, closed, now)
                });
                fetch.due(now, Self::CHECKPOINT_RETRY)
            };
            for from in due {
                self.send_checkpoint_request(
                    CheckpointReq {
                        epoch,
                        value: closed,
                        from,
                    },
                    None,
                );
            }
        }
    }

    fn send_checkpoint_request(&self, request: CheckpointReq, channel: Option<&Arc<Channel>>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::CheckpointReq, Direction::Out);
        let mut flooder = self.flooder.lock().unwrap();
        let message = Message::CheckpointReq(request);
        match channel {
            Some(channel) => {
                let _ = flooder.try_send(channel, &message, TrafficType::Generic);
            }
            None => {
                flooder.flood_prs_and_some_non_prs(&message, TrafficType::Generic, 1.0);
            }
        }
    }

    /// RAI, checkpoint catch-up: a lagging replica asks for a decided state
    /// this node holds; one chunk of its encoding goes back
    pub fn handle_checkpoint_request(&self, request: CheckpointReq, channel: &Arc<Channel>) {
        self.stats
            .inc_dir(StatType::Message, DetailType::CheckpointReq, Direction::In);
        let Some(bytes) = self.checkpoint_bytes(request.epoch, request.value) else {
            return;
        };
        let from = (request.from as usize).min(bytes.len());
        let to = (from + CheckpointReply::MAX_DATA).min(bytes.len());
        let reply = CheckpointReply {
            epoch: request.epoch,
            value: request.value,
            total: bytes.len() as u32,
            from: from as u32,
            data: bytes[from..to].to_vec(),
        };
        self.stats.inc_dir(
            StatType::Message,
            DetailType::CheckpointReply,
            Direction::Out,
        );
        self.flooder.lock().unwrap().try_send(
            channel,
            &Message::CheckpointReply(reply),
            TrafficType::Generic,
        );
    }

    /// The encoding of the value this node decided for an epoch and its
    /// state, if it is the value asked for
    fn checkpoint_bytes(&self, epoch: ConsensusEpoch, value: BlockHash) -> Option<Arc<Vec<u8>>> {
        let mut served = self.served_checkpoint.lock().unwrap();
        if let Some((held, hash, bytes)) = served.as_ref()
            && *held == epoch
            && *hash == value
        {
            return Some(bytes.clone());
        }
        let (decided, state) = self.active_elections.decided_value(epoch)?;
        if decided.hash() != value {
            return None;
        }
        let bytes = Arc::new(checkpoint_bytes(&decided, &state));
        *served = Some((epoch, value, bytes.clone()));
        Some(bytes)
    }

    /// RAI, checkpoint catch-up: a chunk of a decided state this node asked
    /// for. The complete state is checked against `d_e` and installed as
    /// the value its close certificate finalized.
    pub fn handle_checkpoint_reply(&self, reply: CheckpointReply, channel: &Arc<Channel>) {
        self.stats.inc_dir(
            StatType::Message,
            DetailType::CheckpointReply,
            Direction::In,
        );
        let now = self.clock.now();
        let (due, done) = {
            let mut fetches = self.checkpoint_fetches.lock().unwrap();
            let Some(fetch) = fetches.get_mut(&reply.epoch) else {
                return;
            };
            if fetch.value() != reply.value {
                return;
            }
            match fetch.take(reply.total, reply.from, &reply.data) {
                Ok(None) => (fetch.due(now, Self::CHECKPOINT_RETRY), None),
                Ok(Some(decided)) => {
                    let fetch = fetches.remove(&reply.epoch).expect("held above");
                    (Vec::new(), Some((fetch, decided)))
                }
                Err(()) => {
                    fetches.remove(&reply.epoch);
                    diagnostic!(
                        "EPOCH_CATCH_UP_REJECTED epoch={} value={} total={} from={}",
                        reply.epoch,
                        reply.value,
                        reply.total,
                        reply.from
                    );
                    return;
                }
            }
        };
        if let Some((fetch, (value, state))) = done {
            let adopted = self
                .active_elections
                .adopt_certified_epoch_value(value, state);
            diagnostic!(
                "EPOCH_CAUGHT_UP epoch={} value={} bytes={} ms={} adopted={}",
                reply.epoch,
                reply.value,
                reply.total,
                fetch.started().elapsed(now).as_millis(),
                adopted
            );
            return;
        }
        // The next chunks from the node that served this one
        for from in due {
            self.send_checkpoint_request(
                CheckpointReq {
                    epoch: reply.epoch,
                    value: reply.value,
                    from,
                },
                Some(channel),
            );
        }
    }

    fn repeat_proposals(&self) {
        let now = self.clock.now();
        {
            let mut repeated = self.repeated.lock().unwrap();
            if repeated.is_some_and(|last| last.elapsed(now) < Self::REPEAT_INTERVAL) {
                return;
            }
            *repeated = Some(now);
        }
        // A proposal is repeated while its epoch is still tracked, not only
        // while the election is open: a replica that missed it and learned
        // the certificate instead has no other way to obtain the value the
        // certificate decided, and without it that epoch has no state there.
        let mut tracked: Vec<ConsensusEpoch> = self
            .active_elections
            .epoch_closes()
            .into_iter()
            .map(|close| close.epoch)
            .collect();
        tracked.sort();
        if tracked.len() > Self::REPEATED_EPOCHS {
            tracked.drain(..tracked.len() - Self::REPEATED_EPOCHS);
        }
        let repeat: Vec<EpochProp> = {
            let mut proposals = self.proposals.lock().unwrap();
            proposals.retain(|(epoch, _), _| tracked.contains(epoch));
            proposals.values().cloned().collect()
        };
        for prop in repeat {
            self.broadcast(prop);
        }
    }

    fn broadcast(&self, prop: EpochProp) {
        self.stats
            .inc_dir(StatType::Message, DetailType::EpochProp, Direction::Out);
        self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
            &Message::EpochProp(prop),
            TrafficType::Generic,
            1.0,
        );
    }
}

/// The reports this node has reconstructed, as an epoch derivation sees
/// them: those signed against the predecessor checkpoint the derivation
/// starts from
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

/// RAI: whether a manifest holds an exclusion witness for a block in an
/// epoch, counted in the committee that issued that epoch's votes
fn manifest_witness(
    manifest: &Manifest,
    members: &EpochMembers,
    origin: ConsensusEpoch,
    hash: &BlockHash,
) -> bool {
    let Some((committee, order)) = members.get(origin) else {
        return false;
    };
    manifest.exclusion_witness(origin, hash, &committee, &order)
}

/// RAI: the committees of the epochs a check asks about, with their member
/// orders, each read once. The checks ask per claim, and every read takes
/// the AEC lock that vote processing writes under.
struct EpochMembers<'a> {
    active_elections: &'a AecService,
    known: RefCell<HashMap<ConsensusEpoch, Option<(Arc<Committee>, Arc<MemberOrder>)>>>,
}

impl<'a> EpochMembers<'a> {
    fn new(active_elections: &'a AecService) -> Self {
        Self {
            active_elections,
            known: RefCell::new(HashMap::new()),
        }
    }

    fn get(&self, epoch: ConsensusEpoch) -> Option<(Arc<Committee>, Arc<MemberOrder>)> {
        self.known
            .borrow_mut()
            .entry(epoch)
            .or_insert_with(|| {
                let committee = self.active_elections.epoch_committee(epoch)?;
                let order = MemberOrder::of(&committee)?;
                Some((committee, Arc::new(order)))
            })
            .clone()
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

fn report_ref(selection: &ReportSelection) -> ReportRef {
    ReportRef {
        reporter: selection.reporter,
        certified: selection.certified,
        residual: selection.residual,
    }
}

fn selection_of(report: &ReportRef) -> ReportSelection {
    ReportSelection {
        reporter: report.reporter,
        certified: report.certified,
        residual: report.residual,
    }
}
