use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
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
    certificate_memo::CertificateMemo,
    checkpoint_fetch::{CheckpointFetch, checkpoint_bytes},
    chunk_window::ChunkWindow,
    close_memo::{CloseMemo, MemoMiss, PreparedClose},
    close_memo_check_enabled, close_memo_enabled, delta_check_enabled, manifest_claims,
    report_service::inherited_from,
    unjustified,
};
use crate::{
    consensus::{
        AecService,
        active_elections::EpochProposalContext,
        election::{
            AccountSlot, BlockIndex, BuildRules, BuildStateError, CertifiedState, Committee,
            EpochLedger, EpochValue, FreshFinalized, Manifest, ManifestAssembly, MemberOrder,
            ReportIndex, ReportRef, ReportSource, ResidualVotes, SelectedReport, build_state,
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
    fresh: &[FreshFinalized],
) -> Vec<(ConsensusEpoch, BlockHash)> {
    let mut claims = manifest_claims(epoch, states, &|block, entry| {
        inherited_from(previous, block, entry)
    });
    claims.extend(witness_claims(previous, states, index));
    if let Some(anchor) = anchor {
        claims.extend(overlap_claims(anchor, fresh));
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
    fresh: &[FreshFinalized],
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
    let certified = overlap_certified(
        fresh,
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

/// RAI: what a close derives from a selection before its state: the claims
/// its evidence manifest must cover and the fresh finalized blocks, taken
/// from the close memo where it stands for the selection
struct Derivation<'m, 'r> {
    prepared: Option<PreparedClose<'m, 'r>>,
    claims: Vec<(ConsensusEpoch, BlockHash)>,
    fresh: Vec<FreshFinalized>,
    /// For the timing diagnostics: accounts memoized/derived, or why not
    label: String,
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
    /// The certificates the report checks look up, filled in for the
    /// epoch's finalized blocks while it runs
    certificates: Arc<Mutex<CertificateMemo>>,
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
    /// Held for a derivation or a proposal check: they run one at a time
    /// and queue in arrival order, as they did on the report exchange, but
    /// without holding it, which every report message needs
    checking: Mutex<()>,
    /// The proposals received and not checked yet, for the checking thread
    unchecked: Mutex<ProposalQueue>,
    proposal_ready: Condvar,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// The derivation of the epochs not decided here yet, memoized while
    /// each epoch runs, and when each was last refreshed
    memos: Mutex<HashMap<ConsensusEpoch, Arc<CloseMemo>>>,
    memo_refreshed: Mutex<HashMap<ConsensusEpoch, Timestamp>>,
}

/// RAI: the proposals waiting for a check, in arrival order, each value
/// once: a copy of one already waiting adds nothing
#[derive(Default)]
struct ProposalQueue {
    order: VecDeque<BlockHash>,
    waiting: HashMap<BlockHash, (EpochValue, Arc<Channel>)>,
    running: bool,
    stopped: bool,
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

    pub(crate) fn new_null() -> Self {
        Self::new(
            Arc::new(Mutex::new(ReportExchange::new())),
            Arc::new(Mutex::new(CertificateMemo::default())),
            Arc::new(AecService::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            MessageFlooder::new_null(),
            Arc::new(SteadyClock::new_null()),
            Arc::new(Stats::default()),
        )
    }

    pub(crate) fn new(
        exchange: Arc<Mutex<ReportExchange>>,
        certificates: Arc<Mutex<CertificateMemo>>,
        active_elections: Arc<AecService>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
        clock: Arc<SteadyClock>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            exchange,
            certificates,
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
            checking: Mutex::new(()),
            unchecked: Mutex::new(ProposalQueue::default()),
            proposal_ready: Condvar::new(),
            worker: Mutex::new(None),
            memos: Mutex::new(HashMap::new()),
            memo_refreshed: Mutex::new(HashMap::new()),
        }
    }

    /// How often a manifest chunk is asked for again
    const MANIFEST_RETRY: std::time::Duration = std::time::Duration::from_millis(300);
    /// How often the close memo of a running epoch catches up with the
    /// certified view
    const MEMO_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
    /// Certificates looked up ahead of the report checks at a time
    const CERTIFICATE_BATCH: usize = 256;

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
        self.queue_check(value, _channel);
    }

    /// Checks proposals on a thread of their own: a check derives a state and
    /// may fetch a manifest, and a network thread waiting for it holds up
    /// every message behind it
    pub fn start(self: &Arc<Self>) {
        self.unchecked.lock().unwrap().running = true;
        let service = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name("Epoch proposals".to_owned())
            .spawn(move || service.run_checks())
            .unwrap();
        *self.worker.lock().unwrap() = Some(handle);
    }

    pub fn stop(&self) {
        self.unchecked.lock().unwrap().stopped = true;
        self.proposal_ready.notify_all();
        if let Some(handle) = self.worker.lock().unwrap().take() {
            handle.join().unwrap();
        }
    }

    /// A proposal to check: queued for the checking thread, once per value,
    /// or checked here when that thread is not running
    fn queue_check(&self, value: EpochValue, channel: &Arc<Channel>) {
        {
            let mut queue = self.unchecked.lock().unwrap();
            if queue.running && !queue.stopped {
                let hash = value.hash();
                if !queue.waiting.contains_key(&hash) {
                    queue.order.push_back(hash);
                    queue.waiting.insert(hash, (value, channel.clone()));
                    self.proposal_ready.notify_one();
                }
                return;
            }
        }
        self.check_proposal(value, channel);
    }

    /// Checks the proposals in arrival order. While none is waiting, the
    /// thread brings the close memos up to date: the derivation work of a
    /// close, done while its epoch runs.
    fn run_checks(&self) {
        loop {
            let next = {
                let mut queue = self.unchecked.lock().unwrap();
                if queue.stopped {
                    queue.running = false;
                    return;
                }
                match queue.order.pop_front() {
                    Some(hash) => queue.waiting.remove(&hash),
                    None => {
                        let (queue, _) = self
                            .proposal_ready
                            .wait_timeout(queue, Self::MEMO_INTERVAL / 4)
                            .unwrap();
                        drop(queue);
                        None
                    }
                }
            };
            match next {
                Some((value, channel)) => self.check_proposal(value, &channel),
                None => self.refresh_memos(),
            }
        }
    }

    /// RAI: brings the close memo of every epoch not decided here up to
    /// date: from this node's signed report once it holds one, after which
    /// the memo is final, and from the certified view at most once every
    /// `MEMO_INTERVAL` before. A memo derives against the epoch's decided
    /// predecessor and report base, so it starts once they are installed.
    fn refresh_memos(&self) {
        if !close_memo_enabled() {
            return;
        }
        let now = self.clock.now();
        let current = self.active_elections.current_epoch();
        let mut epochs: Vec<ConsensusEpoch> = self
            .active_elections
            .epoch_closes()
            .iter()
            .filter(|close| close.value.is_none())
            .map(|close| close.epoch)
            .collect();
        if !epochs.contains(&current) {
            epochs.push(current);
        }
        for epoch in epochs {
            if self.active_elections.epoch_decided(epoch) {
                continue;
            }
            let (Some(previous), Some(predecessor), Some(base), Some(anchor)) = (
                self.active_elections.epoch_previous_state(epoch),
                self.active_elections.epoch_previous_hash(epoch),
                self.active_elections.report_base(epoch),
                self.overlap_anchor(epoch),
            ) else {
                continue;
            };
            let held = self.memos.lock().unwrap().get(&epoch).cloned();
            let mut memo = match held {
                Some(memo) if memo.derives_against(predecessor, &base) => memo,
                _ => Arc::new(CloseMemo::new(epoch, predecessor, previous, base, anchor)),
            };
            if memo.is_frozen() {
                continue;
            }
            let own = self.exchange.lock().unwrap().own_certified(epoch);
            let due = self
                .memo_refreshed
                .lock()
                .unwrap()
                .get(&epoch)
                .is_none_or(|last| last.elapsed(now) >= Self::MEMO_INTERVAL);
            if own.is_none() && !due {
                continue;
            }
            let started = std::time::Instant::now();
            let frozen = own.is_some();
            // While the epoch runs, only what it finalized since: the
            // elections' lock is held for that, not for the epoch so far.
            // The signed report, once there, is read in full: it holds the
            // certificates of instances not finalized yet as well.
            let (derived, viewed) = match own {
                Some(view) => (
                    Arc::make_mut(&mut memo).refresh(&view, true),
                    std::time::Duration::ZERO,
                ),
                None => {
                    let (finalized, recorded) = self
                        .active_elections
                        .epoch_finalized_since(epoch, memo.absorbed());
                    let viewed = started.elapsed();
                    // The report checks of the close will ask for the
                    // certificates of what the epoch finalized: looked up
                    // now, a batch at a time, they are answered then
                    let fresh: Vec<BlockHash> = finalized
                        .iter()
                        .filter(|(_, _, own_epoch)| *own_epoch)
                        .map(|(block, _, _)| block.hash)
                        .collect();
                    for batch in fresh.chunks(Self::CERTIFICATE_BATCH) {
                        CertificateMemo::kinds_of(
                            &self.certificates,
                            epoch,
                            batch,
                            &|epoch, hashes| self.active_elections.certificate_kinds(epoch, hashes),
                        );
                    }
                    (Arc::make_mut(&mut memo).absorb(finalized, recorded), viewed)
                }
            };
            let (accounts, standing) = memo.counts();
            diagnostic!(
                "EPOCH_MEMO_REFRESH epoch={} frozen={} derived={} accounts={} memoized={} unmemoized={:?} certificates={} view_ms={} ms={}",
                epoch,
                frozen,
                derived,
                accounts,
                standing,
                memo.unmemoized(),
                self.certificates.lock().unwrap().len(),
                viewed.as_millis(),
                started.elapsed().as_millis()
            );
            self.memos.lock().unwrap().insert(epoch, memo);
            self.memo_refreshed.lock().unwrap().insert(epoch, now);
        }
        // A decided epoch's memo is done with
        let decided: Vec<ConsensusEpoch> = self
            .memos
            .lock()
            .unwrap()
            .keys()
            .copied()
            .filter(|epoch| self.active_elections.epoch_decided(*epoch))
            .collect();
        let mut memos = self.memos.lock().unwrap();
        let mut refreshed = self.memo_refreshed.lock().unwrap();
        for epoch in decided {
            memos.remove(&epoch);
            refreshed.remove(&epoch);
        }
        // A report check reads finality back a few epochs, no further
        if let Some(oldest) = current.as_u64().checked_sub(Self::REPEATED_EPOCHS as u64) {
            self.certificates
                .lock()
                .unwrap()
                .forget_before(ConsensusEpoch::new(oldest));
        }
    }

    /// The close memo of an epoch, if it derives against this predecessor
    fn memo_for(&self, epoch: ConsensusEpoch, predecessor: BlockHash) -> Option<Arc<CloseMemo>> {
        if !close_memo_enabled() {
            return None;
        }
        let base = self.active_elections.report_base(epoch)?;
        self.memos
            .lock()
            .unwrap()
            .get(&epoch)
            .filter(|memo| memo.derives_against(predecessor, &base))
            .cloned()
    }

    /// The claims and fresh finalized blocks of a selection, by the memo
    /// where it stands for it, in full otherwise
    fn derivation<'m, 'r>(
        &self,
        memo: Option<&'m CloseMemo>,
        epoch: ConsensusEpoch,
        states: &[SelectedReport<'r>],
        previous: &EpochLedger,
        anchor: Option<&EpochLedger>,
    ) -> Derivation<'m, 'r> {
        let prepared = match memo {
            Some(memo) => memo.prepare(states).map_err(|miss| match miss {
                MemoMiss::OtherBase => "other_base",
                MemoMiss::RepeatedReporter => "repeated_reporter",
                MemoMiss::SharedHash => "shared_hash",
            }),
            None => Err("none"),
        };
        let full = || {
            let fresh = fresh_finalized(states, previous);
            let index = self.report_index(epoch, previous, states);
            let claims = candidate_claims(epoch, states, previous, anchor, &index, &fresh);
            (claims, fresh)
        };
        match prepared {
            Ok(prepared) => {
                let fresh = prepared.fresh();
                let claims = prepared.claims(&fresh);
                let (memoized, derived) = prepared.counts();
                if close_memo_check_enabled() {
                    let (full_claims, full_fresh) = full();
                    diagnostic!(
                        "EPOCH_MEMO_CHECK epoch={} part=claims agrees={} fresh_agrees={}",
                        epoch,
                        full_claims == claims,
                        full_fresh == fresh
                    );
                    if full_claims != claims || full_fresh != fresh {
                        return Derivation {
                            prepared: None,
                            claims: full_claims,
                            fresh: full_fresh,
                            label: "check_failed".to_owned(),
                        };
                    }
                }
                Derivation {
                    prepared: Some(prepared),
                    claims,
                    fresh,
                    label: format!("{memoized}/{derived}"),
                }
            }
            Err(reason) => {
                let (claims, fresh) = full();
                Derivation {
                    prepared: None,
                    claims,
                    fresh,
                    label: reason.to_owned(),
                }
            }
        }
    }

    /// `BuildState(S_{e-1}, Q_e)` of a selection: by the memo where the
    /// derivation was prepared with it, in full otherwise
    fn build(
        &self,
        derivation: &Derivation,
        epoch: ConsensusEpoch,
        states: &[SelectedReport],
        previous: &EpochLedger,
        witness: &dyn Fn(ConsensusEpoch, &BlockHash) -> bool,
    ) -> Result<EpochLedger, BuildStateError> {
        let rules = BuildRules { epoch };
        let full = || {
            let index = self.report_index(epoch, previous, states);
            build_state(previous, states, &index, rules, witness)
        };
        let Some(prepared) = &derivation.prepared else {
            return full();
        };
        let built = prepared.build(witness);
        if close_memo_check_enabled() {
            let whole = full();
            let agrees = match (&built, &whole) {
                (Ok(Some(built)), Ok(whole)) => built == whole,
                (Err(_), Err(_)) | (Ok(None), _) => true,
                _ => false,
            };
            diagnostic!(
                "EPOCH_MEMO_CHECK epoch={} part=state agrees={}",
                epoch,
                agrees
            );
            if !agrees {
                return whole;
            }
        }
        match built? {
            Some(ledger) => Ok(ledger),
            None => full(),
        }
    }

    fn check_proposal(&self, value: EpochValue, channel: &Arc<Channel>) {
        let hash = value.hash();
        let (epoch, slot) = (value.epoch, value.slot);
        match self.validate(&value, Some(channel)) {
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
        let Some(predecessor) = self.active_elections.epoch_previous_hash(epoch) else {
            return false;
        };
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
        let predecessor = self.active_elections.epoch_previous_hash(epoch)?;
        let _checking = self.checking.lock().unwrap();
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
        // Copied out: the derivation runs without holding the exchange
        let source = OwnedReports::copy(&exchange, epoch, &committee, predecessor, &selected);
        drop(exchange);
        let resolved: Vec<(ReportRef, SelectedReport)> = selected
            .iter()
            .filter_map(|report| Some((*report, source.report(report)?)))
            .collect();
        if resolved.len() != selected.len() {
            return None;
        }
        let states: Vec<SelectedReport> = resolved.iter().map(|(_, state)| *state).collect();
        // The evidence this leader used, committed to by digest, with the
        // overlap certificates it proves marked
        let anchor = self.overlap_anchor(epoch)?;
        let started = std::time::Instant::now();
        let memo = self.memo_for(epoch, predecessor);
        let derivation = self.derivation(
            memo.as_deref(),
            epoch,
            &states,
            &previous,
            anchor.as_deref(),
        );
        let claimed = started.elapsed();
        let members = EpochMembers::new(&self.active_elections);
        let manifest = Arc::new(marked_manifest(
            &self.active_elections.evidence_manifest(&derivation.claims),
            epoch,
            &derivation.fresh,
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
        let built = self
            .build(&derivation, epoch, &states, &previous, &witness)
            .map(|ledger| {
                EpochValue::propose_built(epoch, round, BlockHash::ZERO, &resolved, digest, ledger)
            });
        match built {
            Ok((value, ledger)) => {
                diagnostic!(
                    "EPOCH_DERIVE_TIMING epoch={} claims={} overlaps={} claim_ms={} manifest_ms={} build_ms={} memo={}",
                    epoch,
                    derivation.claims.len(),
                    manifest.overlaps().count(),
                    claimed.as_millis(),
                    (manifested - claimed).as_millis(),
                    (started.elapsed() - manifested).as_millis(),
                    derivation.label
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
    /// The index placing the blocks of a derivation: the predecessor's by
    /// the epoch's report base, which holds them all, and the reports' own
    fn report_index(
        &self,
        epoch: ConsensusEpoch,
        previous: &EpochLedger,
        states: &[SelectedReport],
    ) -> ReportIndex {
        match self.active_elections.report_base(epoch) {
            Some(base) => ReportIndex::over_base(&base, states),
            None => ReportIndex::new(previous, states),
        }
    }

    fn validate(&self, value: &EpochValue, channel: Option<&Arc<Channel>>) -> Validation {
        let Some(previous) = self.active_elections.epoch_previous_state(value.epoch) else {
            return Validation::Pending;
        };
        let Some(predecessor) = self.active_elections.epoch_previous_hash(value.epoch) else {
            return Validation::Pending;
        };
        let Some(committee) = self.active_elections.epoch_committee(value.epoch) else {
            return Validation::Pending;
        };
        let _checking = self.checking.lock().unwrap();
        // Copies of a proposal arrive from several peers and its leader
        // repeats it every `REPEAT_INTERVAL`, so several wait here while one
        // is checked, a second or more with a fetched manifest. Once that
        // check has accepted the value, or the epoch is decided, the waiting
        // copies return instead of checking it again.
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
        // The selected reports are copied out, and the exchange released for
        // the report messages while the check runs
        let source = OwnedReports::copy(
            &self.exchange.lock().unwrap(),
            value.epoch,
            &committee,
            predecessor,
            value.reports(),
        );
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
        let memo = self.memo_for(value.epoch, predecessor);
        let derivation = self.derivation(
            memo.as_deref(),
            value.epoch,
            &states,
            &previous,
            anchor.as_deref(),
        );
        match self.evidence_committed(
            value,
            &states,
            &derivation,
            &previous,
            anchor.as_deref(),
            channel,
        ) {
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
        let rules = BuildRules { epoch: value.epoch };
        match value.validate_with(&source, committee.thresholds().report, |states| {
            self.build(&derivation, value.epoch, states, &previous, &witness)
        }) {
            Ok(ledger) => {
                diagnostic!(
                    "EPOCH_VALIDATE_TIMING epoch={} evidence_ms={} build_ms={} memo={}",
                    value.epoch,
                    committed.as_millis(),
                    (started.elapsed() - committed).as_millis(),
                    derivation.label
                );
                if delta_check_enabled() {
                    let index = self.report_index(value.epoch, &previous, &states);
                    check_reports_flat(
                        value.epoch,
                        &previous,
                        &states,
                        &index,
                        anchor.as_deref(),
                        &ledger,
                        rules,
                        &witness,
                    );
                }
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
        derivation: &Derivation,
        previous: &EpochLedger,
        anchor: Option<&EpochLedger>,
        channel: Option<&Arc<Channel>>,
    ) -> Evidence {
        let epoch = value.epoch;
        let members = EpochMembers::new(&self.active_elections);
        let own = marked_manifest(
            &self.active_elections.evidence_manifest(&derivation.claims),
            epoch,
            &derivation.fresh,
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
        let proven = marked_manifest(
            &manifest,
            epoch,
            &derivation.fresh,
            previous,
            anchor,
            &members,
        );
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
/// RAI: the usable reports a derivation or a check selects, copied out of
/// the exchange, so that it runs without holding it: every report message
/// needs the exchange, and a derivation takes long. A report laid over the
/// inherited base is copied in the time of its changes.
struct OwnedReports {
    reports: Vec<(ReportRef, Amount, Arc<CertifiedState>, Arc<ResidualVotes>)>,
}

impl OwnedReports {
    fn copy(
        exchange: &ReportExchange,
        epoch: ConsensusEpoch,
        committee: &Committee,
        predecessor: BlockHash,
        selection: &[ReportRef],
    ) -> Self {
        let reports = selection
            .iter()
            .filter_map(|report| {
                let (certified, residual) = exchange.usable_report_shared(
                    epoch,
                    &report.reporter,
                    report.certified,
                    report.residual,
                    predecessor,
                )?;
                Some((
                    *report,
                    committee.weight(&report.reporter),
                    certified,
                    residual,
                ))
            })
            .collect();
        Self { reports }
    }
}

impl ReportSource for OwnedReports {
    fn report(&self, report: &ReportRef) -> Option<SelectedReport<'_>> {
        let (_, weight, certified, residual) = self.reports.iter().find(|(held, ..)| {
            held.reporter == report.reporter
                && held.certified == report.certified
                && held.residual == report.residual
        })?;
        Some(SelectedReport {
            reporter: report.reporter,
            weight: *weight,
            certified,
            residual,
        })
    }
}

/// RAI: whether a manifest holds an exclusion witness for a block in an
/// epoch, counted in the committee that issued that epoch's votes

/// RAI evaluation: the reports laid over the inherited base are checked
/// and derived from by their changes alone. This derives the same value from
/// flat copies of them, which every check reads in full, and says whether
/// the state and the evidence claims agree.
fn check_reports_flat(
    epoch: ConsensusEpoch,
    previous: &EpochLedger,
    states: &[SelectedReport],
    index: &ReportIndex,
    anchor: Option<&EpochLedger>,
    derived: &EpochLedger,
    rules: BuildRules,
    witness: &dyn Fn(ConsensusEpoch, &BlockHash) -> bool,
) {
    let flat: Vec<CertifiedState> = states
        .iter()
        .map(|state| state.certified.flattened())
        .collect();
    let flat_states: Vec<SelectedReport> = states
        .iter()
        .zip(&flat)
        .map(|(state, certified)| SelectedReport {
            certified,
            ..*state
        })
        .collect();
    let flat_index = ReportIndex::new(previous, &flat_states);
    let state_agrees = build_state(previous, &flat_states, &flat_index, rules, witness)
        .is_ok_and(|ledger| ledger == *derived);
    let claims_agree = candidate_claims(
        epoch,
        states,
        previous,
        anchor,
        index,
        &fresh_finalized(states, previous),
    ) == candidate_claims(
        epoch,
        &flat_states,
        previous,
        anchor,
        &flat_index,
        &fresh_finalized(&flat_states, previous),
    );
    diagnostic!(
        "REPORT_DELTA_CHECK epoch={} reports={} changed={} state_agrees={} claims_agree={}",
        epoch,
        states.len(),
        states
            .iter()
            .map(|state| state.certified.changed_entries().count())
            .sum::<usize>(),
        state_agrees,
        claims_agree
    );
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A proposal waits for the checking thread once per value; without
    /// that thread it is checked at once
    #[test]
    fn a_proposal_is_queued_once_for_the_checking_thread() {
        let service = EpochDecisionService::new_null();
        let channel = Arc::new(Channel::new_test_instance());
        service.queue_check(test_value(1), &channel);
        assert!(service.unchecked.lock().unwrap().waiting.is_empty());

        service.unchecked.lock().unwrap().running = true;
        service.queue_check(test_value(1), &channel);
        service.queue_check(test_value(1), &channel);
        service.queue_check(test_value(2), &channel);
        let queue = service.unchecked.lock().unwrap();
        assert_eq!(queue.waiting.len(), 2);
        assert_eq!(queue.order.len(), 2);
    }

    /// The checking thread waits for proposals and ends when stopped
    #[test]
    fn the_checking_thread_ends_when_stopped() {
        let service = Arc::new(EpochDecisionService::new_null());
        service.start();
        let channel = Arc::new(Channel::new_test_instance());
        service.queue_check(test_value(1), &channel);
        service.stop();
        assert!(!service.unchecked.lock().unwrap().running);
    }

    /* Test helpers */

    fn test_value(slot: u32) -> EpochValue {
        EpochValue::from_parts(
            ConsensusEpoch::new(1),
            slot,
            BlockHash::ZERO,
            vec![ReportRef {
                reporter: rsnano_types::PrivateKey::from(1).public_key(),
                certified: BlockHash::from(1),
                residual: BlockHash::from(2),
            }],
            BlockHash::from(3),
            BlockHash::from(4),
        )
    }
}
