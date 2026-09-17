//! Kudzu close rounds with a rotating close assembler (RAI.tex A0, C0-C4): a
//! drained replica announces its membership sketch for reconciliation and,
//! once close-ready, signs one announcement per round naming the previous
//! close and its membership root (C1). The round's assembler pairs the root a
//! quorum announced with a ParentOK parent from its validated close cache and
//! publishes the signed value (C2); a replica FIRST-votes or notarizes it only
//! after checking the assembler's signature, the announcement certificate,
//! its own previous close, ParentOK and that it holds the target's members.
//! The announcing quorum carries the D3/D4 judgment, so a member learned
//! after the vote never withdraws it. Voting epochs and canonical ledger
//! epochs are independent.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rsnano_ledger::{AnySet, Ledger, MembershipSketch, MembershipTrie, RepWeights};
use rsnano_messages::{CloseSigner, EpochClose, Message};
use rsnano_network::{ChannelId, TrafficType};
use rsnano_types::{
    Amount, BlockHash, ElectionId, PrivateKey, PublicKey, Root, Signature, Vote, VoteKind,
};
use rsnano_utils::{CancellationToken, ticker::Tickable};

use crate::{
    consensus::{
        AecService, VoteGenerators,
        election::kudzu::{KudzuThresholds, KudzuVotes},
    },
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};

// Opt-in, per-process evidence files avoid interleaving the six nodes' stderr.
pub(crate) fn debug_trace(event: impl FnOnce() -> serde_json::Value) {
    use std::{
        fs::{File, OpenOptions},
        io::Write,
        sync::OnceLock,
    };
    static FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    let file = FILE.get_or_init(|| {
        let dir = std::env::var_os("RAI_CLOSE_TRACE_DIR")?;
        let path = std::path::Path::new(&dir).join(format!("{}.jsonl", std::process::id()));
        Some(Mutex::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .expect("open close trace"),
        ))
    });
    if let Some(file) = file {
        let mut file = file.lock().unwrap();
        let record = serde_json::json!({"time_us":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros(),"event":event()});
        writeln!(file, "{record}").expect("write close trace");
    }
}

/// Per-vote and per-request events flood the trace; they are written only when
/// `RAI_CLOSE_TRACE_VERBOSE` is also set.
pub(crate) fn debug_trace_verbose(event: impl FnOnce() -> serde_json::Value) {
    use std::sync::OnceLock;
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    if *VERBOSE.get_or_init(|| std::env::var_os("RAI_CLOSE_TRACE_VERBOSE").is_some()) {
        debug_trace(event);
    }
}

const ROUND_TIMEOUT: Duration = Duration::from_secs(3);
const RETRANSMIT: Duration = Duration::from_secs(2);
/// Packets flooded per tick. A channel write queue holds 64 messages per traffic
/// type and `try_send` drops on overflow, so a synchronous flood of a whole close
/// archive loses the same tail on every retransmission. Bounded bursts at the
/// 100 ms tick cadence stay well below that capacity.
const RETRANSMIT_BURST: usize = 16;
/// Page requests and page replies sent per tick.
const PAGE_BURST: usize = 32;
/// Rounds ahead of the local round whose votes are stored on arrival. Rounds end
/// as fast as votes propagate while digests differ, so a replica that misses one
/// packet would otherwise crawl one round per retransmission cycle behind peers.
const FUTURE_ROUNDS: u64 = 256;
/// Minimum spacing of direct solicitations for one election. Peers answer at once
/// and their reply cache suppresses repeats for five seconds, so a shorter spacing
/// would only add request and reply load to the workload.
const SOLICIT_INTERVAL: Duration = Duration::from_secs(10);
/// Spacing of solicitations for members named by a peer's leaf page. These name
/// exactly what is missing, so they are few.
const RECONCILE_INTERVAL: Duration = RETRANSMIT;
/// The announcement phase of round 0: a close-ready replica signs its one
/// commitment for the round once every representative's sketch names its own
/// pair, or after this long since its own readiness. A replica holding more
/// members than the committed value still votes it, so the wait only gives
/// late certificates a chance to enter the close instead of being discarded
/// with it.
const AGREEMENT_TIMEOUT: Duration = Duration::from_secs(6);
/// Proposals stored per round. A correct assembler signs one value per round;
/// an equivocating one may pair the certified target with several parents,
/// and a bounded number of them can still be notarized on second look.
const PROPOSALS_PER_ROUND: usize = 8;
static DROPPED_CLOSE_PACKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The first-vote budget of a close round doubles every round without bound
/// while the epoch stays open: the termination argument needs a round whose
/// budget exceeds entry skew, delivery and validation, and a cap would let
/// unboundedly slow conditions defeat every round.
fn round_timeout(round: u64) -> Duration {
    let factor = u32::try_from(round)
        .ok()
        .and_then(|round| 1u64.checked_shl(round))
        .unwrap_or(u64::MAX);
    Duration::from_secs(ROUND_TIMEOUT.as_secs().saturating_mul(factor))
}

fn epoch_due(start: Instant, now: Instant, epoch: u64, seconds: u64) -> bool {
    now >= start
        && now.duration_since(start).as_secs() >= seconds.saturating_mul(epoch.saturating_add(1))
}

#[derive(Default)]
struct Signer {
    first: Option<BlockHash>,
    notarized: HashSet<BlockHash>,
    final_vote: Option<BlockHash>,
    timeout: bool,
    /// This key, as the round's assembler, signed its one proposal.
    proposed: bool,
}
struct Round {
    tally: KudzuVotes,
    votes: BTreeMap<(PublicKey, u8, BlockHash), EpochClose>,
    signers: HashMap<PublicKey, Signer>,
    /// The one C1 commitment of each representative in this round; a second
    /// one from the same signer is equivocation and is ignored.
    commitments: HashMap<PublicKey, EpochClose>,
    started: Instant,
}
impl Default for Round {
    fn default() -> Self {
        Self {
            tally: Default::default(),
            votes: Default::default(),
            signers: Default::default(),
            commitments: Default::default(),
            started: Instant::now(),
        }
    }
}
impl Round {
    fn receive(&mut self, packet: EpochClose, weights: &RepWeights, total: Amount) -> bool {
        let id = packet.candidate_id();
        let mut vote = Vote::null();
        vote.kind = kind(packet.kind);
        vote.epoch = packet.round;
        vote.voter = packet.voter;
        vote.hashes = vec![id];
        if self.tally.insert(Arc::new(vote), id).is_ok() {
            self.votes.insert((packet.voter, packet.kind, id), packet);
            self.tally.tally(weights, total);
            true
        } else {
            false
        }
    }
    fn certificate(&self, id: BlockHash, kind: VoteKind) -> bool {
        self.tally.has_certificate(id, kind)
    }
}
fn kind(k: u8) -> VoteKind {
    match k {
        0 => VoteKind::First,
        1 => VoteKind::Notarize,
        2 => VoteKind::Final,
        3 => VoteKind::FirstTimeout,
        _ => VoteKind::Timeout,
    }
}
struct Candidate {
    header: EpochClose,
    /// Channels that delivered votes for it, asked for the sketch and pages of
    /// its view when it finalizes with a root this replica never held.
    sources: Vec<ChannelId>,
    /// The round assembler's signed proposal of this value (C2(a)), with the
    /// announcements certifying its target; retained and rebroadcast so a
    /// second-look replica can run the same checks.
    proposal: Option<EpochClose>,
}

/// What this replica has learned about a membership named by its root: the
/// sketch from an announcement and the members it decoded on either side, or,
/// when the difference was too large to decode, the level-1 digests, level-2
/// pages and leaves fetched for the buckets that differ, and where to ask.
#[derive(Default)]
struct View {
    sketch: Option<MembershipSketch>,
    /// Local root the sketch was last decoded against; the difference is
    /// relative to that membership.
    decoded_for: Option<BlockHash>,
    sketch_failed: bool,
    only_mine: Vec<BlockHash>,
    only_theirs: Vec<BlockHash>,
    level1: Option<Vec<BlockHash>>,
    level2: HashMap<u8, Vec<BlockHash>>,
    /// Members received per prefix and the leaf size the sender declared.
    leaves: HashMap<u16, (BTreeSet<BlockHash>, usize)>,
    sources: Vec<ChannelId>,
    requested: HashMap<(u16, u16), Instant>,
    /// Live root and page count of the last assembly attempt, so the whole
    /// membership is not copied again while nothing changed.
    attempted: Option<(BlockHash, usize)>,
    /// Whether every member of this view is held here, for the live root it
    /// was last checked against.
    subset: Option<(BlockHash, bool)>,
}
impl View {
    /// The sketch decoded against the live membership named by `root`.
    fn decoded(&self, root: BlockHash) -> bool {
        self.decoded_for == Some(root)
    }
    /// Pages are fetched only for a view whose sketch did not decode.
    fn needs_pages(&self) -> bool {
        self.sketch_failed || self.sketch.is_none()
    }
    fn add_source(&mut self, channel: ChannelId) {
        if !self.sources.contains(&channel) && self.sources.len() < 16 {
            self.sources.push(channel);
        }
    }
    fn leaf_complete(&self, prefix: u16) -> Option<&BTreeSet<BlockHash>> {
        self.leaves
            .get(&prefix)
            .filter(|(members, size)| members.len() >= *size)
            .map(|(members, _)| members)
    }
}

/// The membership a close was persisted with, kept so lagging replicas can
/// still fetch the pages of that view and reconstruct it exactly.
struct ClosedView {
    trie: MembershipTrie,
    announcements: Vec<EpochClose>,
}

struct State {
    draining: bool,
    /// This replica's FIRST obligations in `epoch`, frozen when the drain began.
    local_first: Arc<Vec<ElectionId>>,
    epoch: u64,
    round: u64,
    previous_close: BlockHash,
    ready: bool,
    rounds: BTreeMap<u64, Round>,
    candidates: BTreeMap<BlockHash, Candidate>,
    weights: RepWeights,
    total: Amount,
    last_send: Instant,
    archive: Vec<EpochClose>,
    receipts: BTreeMap<(u64, PublicKey), BlockHash>,
    local_receipts: Vec<EpochClose>,
    retransmit: VecDeque<EpochClose>,
    solicited: HashMap<BlockHash, Instant>,
    last_timeout_solicitation: Option<Instant>,
    /// Position in the drain recovery targets for the next solicitation.
    recovery_cursor: usize,
    /// The readiness trace event was written for this epoch.
    ready_traced: bool,
    /// When this replica first completed its drain.
    ready_since: Option<Instant>,
    /// Round 0 is timed from the moment a proposal became due, not from the
    /// drain start; later rounds from their timeout certificate.
    round_timer_armed: bool,
    /// Live membership of `epoch`, followed incrementally from the ledger.
    trie: MembershipTrie,
    /// Ledger candidate log position consumed into `trie`.
    log_index: usize,
    closed: Option<ClosedView>,
    /// Latest membership sketch of every representative and its channel.
    announcements: HashMap<PublicKey, (EpochClose, ChannelId)>,
    local_announcements: Vec<EpochClose>,
    announcement_seq: u64,
    /// This replica's commitments in this epoch, one per round, retransmitted
    /// with the close history.
    local_commitments: Vec<EpochClose>,
    views: HashMap<BlockHash, View>,
    pages_out: VecDeque<(ChannelId, EpochClose)>,
    /// Members held by peers but not here, with the last solicitation.
    reconcile_targets: HashMap<BlockHash, (Root, Option<Instant>)>,
}
impl State {
    fn new(epoch: u64, weights: RepWeights, archive: Vec<EpochClose>) -> Self {
        let total = Amount::raw(
            weights
                .values()
                .fold(0u128, |s, w| s.saturating_add(w.number())),
        );
        Self {
            draining: false,
            local_first: Arc::new(Vec::new()),
            epoch,
            round: 0,
            previous_close: BlockHash::ZERO,
            ready: false,
            rounds: Default::default(),
            candidates: Default::default(),
            weights,
            total,
            last_send: Instant::now() - RETRANSMIT,
            archive,
            receipts: BTreeMap::new(),
            local_receipts: Vec::new(),
            retransmit: Default::default(),
            solicited: Default::default(),
            last_timeout_solicitation: None,
            recovery_cursor: 0,
            ready_traced: false,
            ready_since: None,
            round_timer_armed: false,
            trie: MembershipTrie::new(epoch),
            log_index: 0,
            closed: None,
            announcements: Default::default(),
            local_announcements: Vec::new(),
            announcement_seq: 0,
            local_commitments: Vec::new(),
            views: Default::default(),
            pages_out: Default::default(),
            reconcile_targets: Default::default(),
        }
    }
    /// One request's worth of recovery targets, rotating through the list so
    /// every election gets its turn: taking the first due entries would starve
    /// the tail as soon as the list outgrows what one throttle interval covers.
    fn next_solicitation_batch(
        &mut self,
        targets: &[(BlockHash, Root)],
        now: Instant,
    ) -> Vec<(BlockHash, Root)> {
        let mut batch = Vec::new();
        if targets.is_empty() {
            self.recovery_cursor = 0;
            return batch;
        }
        let start = self.recovery_cursor % targets.len();
        let mut examined = 0;
        while examined < targets.len() && batch.len() < rsnano_messages::ConfirmReq::HASHES_MAX {
            let (hash, root) = targets[(start + examined) % targets.len()].clone();
            examined += 1;
            if self.solicitation_due(hash, now) {
                batch.push((hash, root));
            }
        }
        self.recovery_cursor = (start + examined) % targets.len();
        batch
    }
    /// Record a direct solicitation of `hash` unless one was sent recently.
    fn solicitation_due(&mut self, hash: BlockHash, now: Instant) -> bool {
        if self
            .solicited
            .get(&hash)
            .is_some_and(|last| now.duration_since(*last) < SOLICIT_INTERVAL)
        {
            return false;
        }
        self.solicited.insert(hash, now);
        true
    }

    fn enter_round(&mut self, round: u64) {
        self.round = round;
        self.rounds.entry(round).or_default().started = Instant::now();
        // A later round follows a timeout certificate every replica observed at
        // about the same time. Round 0 waits for the proposal to become due.
        self.round_timer_armed = round > 0;
    }
    fn arm_round_timer(&mut self) {
        self.rounds.entry(self.round).or_default().started = Instant::now();
        self.round_timer_armed = true;
    }
    fn round_timed_out(&self) -> bool {
        self.round_timer_armed
            && self
                .rounds
                .get(&self.round)
                .is_some_and(|r| r.started.elapsed() >= round_timeout(self.round))
    }

    /// Follow the ledger's candidate log into the live membership. Returns the
    /// members added.
    fn sync_membership(&mut self, ledger: &Ledger) -> Vec<BlockHash> {
        let (added, next) = ledger.epoch_candidates_since(self.epoch, self.log_index);
        self.log_index = next;
        for member in &added {
            self.trie.insert(*member);
        }
        added
    }
    fn root(&mut self) -> BlockHash {
        self.trie.root()
    }

    /// The close assembler of `round` (rule A0): the committee in public key
    /// order, round-robin from a start the epoch fixes. The role only decides
    /// who publishes the round's value; every voter checks it independently.
    fn assembler(&self, round: u64) -> Option<PublicKey> {
        let committee: BTreeSet<_> = self
            .weights
            .iter()
            .filter(|(_, weight)| !weight.is_zero())
            .map(|(rep, _)| *rep)
            .collect();
        let n = committee.len() as u64;
        if n == 0 {
            return None;
        }
        let position = (round % n + self.epoch % n) % n;
        committee.iter().nth(position as usize).copied()
    }
    /// Announcement weight `state` collected in `round`.
    fn announced_weight(&self, round: u64, state: BlockHash) -> Amount {
        let Some(round) = self.rounds.get(&round) else {
            return Amount::raw(0);
        };
        Amount::raw(
            round
                .commitments
                .values()
                .filter(|commitment| commitment.state == state)
                .fold(0u128, |sum, commitment| {
                    sum.saturating_add(self.weights.weight(&commitment.voter).number())
                }),
        )
    }
    /// C2(b): certificate weight of the announcements of `round` names
    /// `state`. Our own keys count through our announcement only, so an
    /// unready replica attests nothing.
    fn certified_target(&self, round: u64, state: BlockHash) -> bool {
        self.announced_weight(round, state) >= KudzuThresholds::new(self.total).certificate
    }
    /// The target root certified in the current round, if any: the round's
    /// unique certifiable pair (Lemma 12), since each representative
    /// announces once per round.
    fn enabled(&self) -> Option<BlockHash> {
        let round = self.rounds.get(&self.round)?;
        round
            .commitments
            .values()
            .map(|commitment| commitment.state)
            .find(|state| self.certified_target(self.round, *state))
    }
    /// The K9 selector: the notarized candidate of the newest round with a
    /// timeout certificate for every later round, smallest candidate id
    /// first; without one, the genesis position. A voter never recomputes
    /// it; it checks ParentOK for the parent the assembler named.
    fn select_parent(&self) -> BlockHash {
        self.candidates
            .iter()
            .filter(|(id, c)| {
                c.header.round < self.round
                    && self.certified(**id, VoteKind::Notarize)
                    && self.timed_out(c.header.round + 1..self.round)
            })
            .min_by_key(|(id, c)| (std::cmp::Reverse(c.header.round), **id))
            .map_or(BlockHash::ZERO, |(id, _)| *id)
    }
    /// Whether the round-0 announcement phase has expired since this replica
    /// became ready.
    fn agreement_expired(&self, now: Instant) -> bool {
        self.ready
            && self
                .ready_since
                .is_some_and(|since| now.duration_since(since) >= AGREEMENT_TIMEOUT)
    }
    /// Every representative's sketch names the root this replica would
    /// announce.
    fn converged(&mut self) -> bool {
        let root = self.root();
        self.weights
            .iter()
            .filter(|(_, weight)| !weight.is_zero())
            .all(|(rep, _)| {
                self.announcements
                    .get(rep)
                    .is_some_and(|(a, _)| a.state == root)
            })
    }
    /// Whether the announcement phase of the current round has ended for this
    /// replica (rule C1): it is close-ready and either every representative's
    /// sketch names its root, or the phase expired, `AGREEMENT_TIMEOUT` after
    /// readiness in round 0 and half the first-vote budget after entry in
    /// later rounds, so that the commitment precedes any FIRST timeout.
    fn commit_due(&mut self, now: Instant) -> bool {
        if !self.ready {
            return false;
        }
        if self.converged() {
            return true;
        }
        if self.round == 0 {
            return self.agreement_expired(now);
        }
        let budget = AGREEMENT_TIMEOUT.min(round_timeout(self.round) / 2);
        self.rounds
            .get(&self.round)
            .is_some_and(|round| now.duration_since(round.started) >= budget)
    }
    /// Sign this replica's one announcement of the current round (C1): the
    /// previous close and its live membership root, no parent. A changed
    /// membership is never announced again in the same round; the next round
    /// announces it.
    fn commit(&mut self, keys: &[PrivateKey], out: &mut Vec<EpochClose>) {
        let root = self.root();
        let mut commitment = self.template(self.round, BlockHash::ZERO, root);
        commitment.kind = 10;
        commitment.members = 0;
        for key in keys {
            let round = self.rounds.entry(self.round).or_default();
            if round.commitments.contains_key(&key.public_key()) {
                continue;
            }
            commitment.sign(key);
            round
                .commitments
                .insert(key.public_key(), commitment.clone());
            self.local_commitments.push(commitment.clone());
            debug_trace(
                || serde_json::json!({"type":"commitment","epoch":self.epoch,"round":self.round,"state":commitment.state,"rep":key.public_key()}),
            );
            out.push(commitment.clone());
        }
    }
    /// Store a peer's commitment for its round; only its first one counts.
    fn receive_commitment(&mut self, p: EpochClose) {
        if p.epoch != self.epoch
            || p.previous_close != self.previous_close
            || p.round > self.round + FUTURE_ROUNDS
            || self.weights.weight(&p.voter).is_zero()
            || !p.valid_commitment()
        {
            return;
        }
        self.rounds
            .entry(p.round)
            .or_default()
            .commitments
            .entry(p.voter)
            .or_insert(p);
    }
    /// Assembly (rule C2): when one of `keys` is the current round's
    /// assembler and announcement weight certified a target, pair the target
    /// with the parent the K9 selector picks from the validated close cache,
    /// sign the one proposal of the round and publish it with the
    /// announcements that certify the target. The assembler need not hold
    /// the target's members itself; as a voter it checks that like anyone.
    fn propose(&mut self, keys: &[PrivateKey], out: &mut Vec<EpochClose>) {
        let Some(assembler) = self.assembler(self.round) else {
            return;
        };
        let Some(key) = keys.iter().find(|key| key.public_key() == assembler) else {
            return;
        };
        let Some(state) = self.enabled() else {
            return;
        };
        let round = self.rounds.entry(self.round).or_default();
        let signer = round.signers.entry(assembler).or_default();
        if signer.proposed {
            return;
        }
        signer.proposed = true;
        // The heaviest announcements first, so a bounded certificate still
        // carries certificate weight.
        let mut certifying: Vec<_> = round
            .commitments
            .values()
            .filter(|commitment| commitment.state == state)
            .collect();
        certifying.sort_by_key(|commitment| {
            (
                std::cmp::Reverse(self.weights.weight(&commitment.voter)),
                commitment.voter,
            )
        });
        let certificate = certifying
            .into_iter()
            .take(EpochClose::CERTIFICATE_MAX)
            .map(|commitment| CloseSigner {
                voter: commitment.voter,
                signature: commitment.signature.clone(),
            })
            .collect();
        let parent = self.select_parent();
        let mut proposal = self.template(self.round, parent, state);
        proposal.kind = 11;
        proposal.certificate = certificate;
        proposal.sign(key);
        debug_trace(
            || serde_json::json!({"type":"proposal","epoch":self.epoch,"round":self.round,"id":proposal.candidate_id(),"parent":parent,"state":state,"rep":assembler}),
        );
        self.receive_proposal(proposal.clone());
        out.push(proposal);
    }
    /// Store the round assembler's proposal (C2(a), with C2(c) for the
    /// previous close) and the announcements it carries. A proposal by anyone
    /// but the round's assembler is ignored, as is any beyond the
    /// equivocation bound of a round; the first proposal of a value is kept.
    fn receive_proposal(&mut self, p: EpochClose) {
        if p.epoch != self.epoch
            || p.previous_close != self.previous_close
            || p.round > self.round + FUTURE_ROUNDS
            || self.assembler(p.round) != Some(p.voter)
            || !p.valid_proposal()
        {
            return;
        }
        for signer in &p.certificate {
            self.receive_commitment(p.commitment_of(signer));
        }
        let id = p.candidate_id();
        if !self.candidates.contains_key(&id)
            && self
                .candidates
                .values()
                .filter(|c| c.header.round == p.round && c.proposal.is_some())
                .count()
                >= PROPOSALS_PER_ROUND
        {
            return;
        }
        let mut header = p.clone();
        header.certificate.clear();
        let candidate = self.candidates.entry(id).or_insert_with(|| Candidate {
            header,
            sources: Vec::new(),
            proposal: None,
        });
        if candidate.proposal.is_none() {
            candidate.proposal = Some(p);
        }
    }

    /// Announce the membership sketch this replica would commit to, its live
    /// membership root, once it changes, so peers can reconcile towards it.
    /// Sketches never certify a value; they are retransmitted with the close
    /// history until the epoch closes.
    fn announce(&mut self, keys: &[PrivateKey], out: &mut Vec<EpochClose>) {
        if !self.ready || keys.is_empty() {
            return;
        }
        let root = self.root();
        if self
            .local_announcements
            .first()
            .is_some_and(|announcement| announcement.state == root)
        {
            return;
        }
        self.announcement_seq += 1;
        let mut announcement = self.template(self.announcement_seq, BlockHash::ZERO, root);
        announcement.kind = 8;
        announcement.set_sketch(&self.trie.sketch().to_bytes());
        self.local_announcements.clear();
        for key in keys {
            announcement.sign(key);
            self.announcements.insert(
                key.public_key(),
                (announcement.clone(), ChannelId::LOOPBACK),
            );
            self.local_announcements.push(announcement.clone());
            out.push(announcement.clone());
        }
    }
    /// Store a peer's announcement. While its root differs from ours it is a
    /// view whose sketch is decoded against the local membership, or whose
    /// differing pages are requested from the announcer when that fails.
    fn receive_announcement(&mut self, p: EpochClose, channel: ChannelId) {
        if p.epoch != self.epoch
            || p.previous_close != self.previous_close
            || self.weights.weight(&p.voter).is_zero()
            || !p.valid_announcement()
            || self
                .announcements
                .get(&p.voter)
                .is_some_and(|(old, _)| old.round >= p.round)
        {
            return;
        }
        self.announcements.insert(p.voter, (p, channel));
    }
    /// The membership a peer may ask pages of: the live one or the last close.
    fn view_trie(&mut self, root: BlockHash) -> Option<&mut MembershipTrie> {
        if self.trie.root() == root {
            return Some(&mut self.trie);
        }
        let closed = self.closed.as_mut()?;
        (closed.trie.root() == root).then_some(&mut closed.trie)
    }
    /// Answer a page request for a view this replica holds.
    fn serve_request(&mut self, p: EpochClose, channel: ChannelId, key: &PrivateKey) {
        if self.weights.weight(&p.voter).is_zero()
            || !p.valid_view_request()
            || self.pages_out.len() >= 256
        {
            return;
        }
        let previous_close = self.previous_close;
        let Some(trie) = self.view_trie(p.state) else {
            return;
        };
        let epoch = trie.epoch();
        let mut pages = Vec::new();
        if p.pages == EpochClose::LEVEL1_PAGE {
            let mut page = p.clone();
            page.kind = 5;
            page.hashes = trie.level1().to_vec();
            page.members = trie.len() as u64;
            pages.push(page);
        } else if p.pages == EpochClose::LEVEL2_PAGE {
            let mut page = p.clone();
            page.kind = 5;
            page.hashes = trie.level2(p.page as u8);
            page.members = trie.len() as u64;
            pages.push(page);
        } else {
            let leaf = trie.leaf(p.page);
            let mut chunks = leaf.chunks(EpochClose::PAGE_SIZE).enumerate().peekable();
            if chunks.peek().is_none() {
                let mut page = p.clone();
                page.kind = 7;
                page.members = 0;
                pages.push(page);
            }
            for (chunk, hashes) in chunks {
                let mut page = p.clone();
                page.kind = 7;
                page.round = chunk as u64;
                page.hashes = hashes.to_vec();
                page.members = leaf.len() as u64;
                pages.push(page);
            }
        }
        for mut page in pages {
            page.epoch = epoch;
            page.previous_close = previous_close;
            page.sign(key);
            self.pages_out.push_back((channel, page));
        }
    }
    /// A level-1 or level-2 digest page of a view.
    fn receive_digest_page(&mut self, p: EpochClose) {
        if self.weights.weight(&p.voter).is_zero() {
            return;
        }
        if p.valid_level1_page() {
            if let Some(view) = self.views.get_mut(&p.state) {
                view.level1 = Some(p.hashes);
            }
        } else if p.valid_level2_page() {
            if let Some(view) = self.views.get_mut(&p.state) {
                view.level2.insert(p.page as u8, p.hashes);
            }
        }
    }
    /// Decode the sketch of every active view against the live membership.
    /// Members held by the peer only become reconciliation targets; a
    /// difference too large to decode falls back to the digest pages.
    fn decode_sketches(&mut self, missing: impl Fn(&[BlockHash]) -> Vec<(BlockHash, Root)>) {
        let root = self.root();
        let epoch = self.epoch;
        let mut targets = Vec::new();
        for (state, view) in &mut self.views {
            if view.decoded(root) || view.sketch_failed {
                continue;
            }
            let Some(sketch) = &view.sketch else {
                continue;
            };
            match self.trie.sketch().difference(sketch) {
                Some((only_mine, only_theirs)) => {
                    debug_trace(
                        || serde_json::json!({"type":"sketch_decoded","epoch":epoch,"state":state,"only_mine":only_mine.len(),"only_theirs":only_theirs.len()}),
                    );
                    targets.extend(only_theirs.iter().copied());
                    view.only_mine = only_mine;
                    view.only_theirs = only_theirs;
                    view.decoded_for = Some(root);
                }
                None => {
                    debug_trace(
                        || serde_json::json!({"type":"sketch_undecodable","epoch":epoch,"state":state}),
                    );
                    view.sketch_failed = true;
                }
            }
        }
        for (hash, root) in missing(&targets) {
            self.reconcile_targets.entry(hash).or_insert((root, None));
        }
    }
    /// Members of a peer's leaf that carry no local membership become
    /// reconciliation targets. Roots unknown locally stay zero: the peer then
    /// publishes the block together with its votes.
    fn receive_leaf_page(
        &mut self,
        p: EpochClose,
        missing: impl Fn(&[BlockHash]) -> Vec<(BlockHash, Root)>,
    ) {
        if self.weights.weight(&p.voter).is_zero() || !p.valid_leaf_page() {
            return;
        }
        let Some(view) = self.views.get_mut(&p.state) else {
            return;
        };
        let (members, size) = view.leaves.entry(p.page).or_default();
        *size = p.members as usize;
        members.extend(p.hashes.iter().copied());
        for (hash, root) in missing(&p.hashes) {
            self.reconcile_targets.entry(hash).or_insert((root, None));
        }
    }
    /// Roots whose difference is worth resolving: the announced memberships
    /// that differ from ours and the finalized candidates this replica never
    /// held.
    fn active_views(&mut self) -> Vec<BlockHash> {
        let root = self.root();
        let mut active = BTreeSet::new();
        for (announcement, channel) in self.announcements.values() {
            if announcement.state == root {
                continue;
            }
            let view = self.views.entry(announcement.state).or_default();
            if view.sketch.is_none() {
                view.sketch = announcement
                    .sketch_bytes()
                    .and_then(|bytes| MembershipSketch::from_bytes(&bytes));
            }
            view.add_source(*channel);
            active.insert(announcement.state);
        }
        if let Some(id) = self.decision() {
            let state = self.candidates[&id].header.state;
            if state != root {
                let sources = self.candidates[&id].sources.clone();
                let view = self.views.entry(state).or_default();
                for source in sources {
                    view.add_source(source);
                }
                active.insert(state);
            }
        }
        self.views.retain(|state, _| active.contains(state));
        active.into_iter().collect()
    }
    /// Requests for the pages of every active view whose sketch did not
    /// decode, for the buckets that differ from the live membership and were
    /// not requested recently, `PAGE_BURST` at most.
    fn page_requests(&mut self, key: &PrivateKey, now: Instant) -> Vec<(ChannelId, EpochClose)> {
        let mut requests = Vec::new();
        let epoch = self.epoch;
        let previous_close = self.previous_close;
        for state in self.active_views() {
            let mut wanted = Vec::new();
            {
                let view = self.views.get_mut(&state).unwrap();
                if view.sources.is_empty() || !view.needs_pages() {
                    continue;
                }
                let Some(level1) = view.level1.clone() else {
                    wanted.push((EpochClose::LEVEL1_PAGE, 0));
                    let view = self.views.get_mut(&state).unwrap();
                    Self::push_page_requests(
                        &mut requests,
                        view,
                        wanted,
                        epoch,
                        previous_close,
                        state,
                        key,
                        now,
                    );
                    continue;
                };
                for bucket in self.trie.mismatched_buckets(&level1) {
                    match view.level2.get(&bucket) {
                        None => wanted.push((EpochClose::LEVEL2_PAGE, bucket as u16)),
                        Some(level2) => {
                            for prefix in self.trie.mismatched_leaves(bucket, level2) {
                                if view.leaf_complete(prefix).is_none() {
                                    wanted.push((EpochClose::LEAF_PAGE, prefix));
                                }
                            }
                        }
                    }
                }
            }
            let view = self.views.get_mut(&state).unwrap();
            Self::push_page_requests(
                &mut requests,
                view,
                wanted,
                epoch,
                previous_close,
                state,
                key,
                now,
            );
            if requests.len() >= PAGE_BURST {
                break;
            }
        }
        requests
    }
    #[allow(clippy::too_many_arguments)]
    fn push_page_requests(
        requests: &mut Vec<(ChannelId, EpochClose)>,
        view: &mut View,
        wanted: Vec<(u16, u16)>,
        epoch: u64,
        previous_close: BlockHash,
        state: BlockHash,
        key: &PrivateKey,
        now: Instant,
    ) {
        for (level, page) in wanted {
            if requests.len() >= PAGE_BURST {
                return;
            }
            if view
                .requested
                .get(&(level, page))
                .is_some_and(|last| now.duration_since(*last) < RETRANSMIT)
            {
                continue;
            }
            view.requested.insert((level, page), now);
            let source = view.sources[view.requested.len() % view.sources.len()];
            let mut request = EpochClose {
                epoch,
                round: 0,
                parent: BlockHash::ZERO,
                previous_close,
                state,
                kind: 9,
                voter: PublicKey::ZERO,
                signature: Signature::new(),
                page,
                pages: level,
                hashes: vec![],
                base: BlockHash::ZERO,
                removed: vec![],
                members: 0,
                sketch: String::new(),
                certificate: vec![],
            };
            request.sign(key);
            requests.push((source, request));
        }
    }
    /// The members of a finalized view this replica never held: its own
    /// membership without the members the view lacks, once every member the
    /// view names is held here. The decoded sketch names both sides directly;
    /// otherwise the differing leaves must all have been fetched.
    fn assemble_view(&mut self, state: BlockHash) -> Option<Vec<BlockHash>> {
        let root = self.root();
        let view = self.views.get_mut(&state)?;
        if view.decoded(root) {
            if !view.only_theirs.is_empty() || view.attempted == Some((root, usize::MAX)) {
                return None;
            }
            view.attempted = Some((root, usize::MAX));
            let excluded: HashSet<_> = view.only_mine.iter().copied().collect();
            let mut assembled = self.trie.without(&excluded);
            return (assembled.root() == state).then(|| assembled.members().copied().collect());
        }
        let attempt = (root, view.level2.len() + view.leaves.len());
        if view.attempted == Some(attempt) {
            return None;
        }
        view.attempted = Some(attempt);
        let level1 = view.level1.clone()?;
        let mut excluded = HashSet::new();
        for bucket in self.trie.mismatched_buckets(&level1) {
            let level2 = self.views.get(&state)?.level2.get(&bucket)?.clone();
            for prefix in self.trie.mismatched_leaves(bucket, &level2) {
                let theirs = self.views.get(&state)?.leaf_complete(prefix)?;
                if theirs.iter().any(|member| !self.trie.contains(member)) {
                    return None;
                }
                excluded.extend(
                    self.trie
                        .leaf(prefix)
                        .iter()
                        .filter(|member| !theirs.contains(member))
                        .copied(),
                );
            }
        }
        let mut assembled = self.trie.without(&excluded);
        (assembled.root() == state).then(|| assembled.members().copied().collect())
    }
    /// Reconciliation targets due for a solicitation. Targets that gained local
    /// membership since are dropped.
    fn reconcile_batch(
        &mut self,
        now: Instant,
        still_missing: impl Fn(&[BlockHash]) -> HashSet<BlockHash>,
    ) -> Vec<(BlockHash, Root)> {
        if self.reconcile_targets.is_empty() {
            return Vec::new();
        }
        let hashes: Vec<_> = self.reconcile_targets.keys().copied().collect();
        let missing = still_missing(&hashes);
        self.reconcile_targets
            .retain(|hash, _| missing.contains(hash));
        let mut batch = Vec::new();
        for (hash, (root, last)) in &mut self.reconcile_targets {
            if batch.len() == rsnano_messages::ConfirmReq::HASHES_MAX {
                break;
            }
            if last.is_none_or(|last| now.duration_since(last) >= RECONCILE_INTERVAL) {
                *last = Some(now);
                batch.push((*hash, *root));
            }
        }
        batch
    }
    /// Queue everything a lagging peer may still need, newest rounds first so the
    /// votes that decide the current round are delivered before older history.
    /// A cycle still in progress is finished before a new one starts, so every
    /// packet is eventually sent regardless of archive size.
    fn queue_retransmission(&mut self) {
        if !self.retransmit.is_empty() {
            return;
        }
        self.retransmit.extend(self.local_receipts.iter().cloned());
        self.retransmit
            .extend(self.local_announcements.iter().cloned());
        self.retransmit
            .extend(self.local_commitments.iter().cloned());
        self.retransmit
            .extend(self.candidates.values().filter_map(|c| c.proposal.clone()));
        self.retransmit.extend(
            self.rounds
                .values()
                .rev()
                .flat_map(|r| r.votes.values().cloned()),
        );
        self.retransmit.extend(self.archive.iter().rev().cloned());
    }
    fn retransmission_burst(&mut self) -> Vec<EpochClose> {
        let count = self.retransmit.len().min(RETRANSMIT_BURST);
        self.retransmit.drain(..count).collect()
    }
    /// Move to the next epoch after closing this one with `hashes`. The closed
    /// membership stays available as a view, announced together with the vote
    /// archive until every representative acknowledged the close, so a lagging
    /// replica can fetch exactly the members it lacks.
    fn advance(&mut self, hashes: Vec<BlockHash>, weights: RepWeights, keys: &[PrivateKey]) {
        let mut archive = std::mem::take(&mut self.archive);
        archive.extend(self.packets());
        let receipts = std::mem::take(&mut self.receipts);
        let local_receipts = std::mem::take(&mut self.local_receipts);
        let epoch = self.epoch;
        let mut trie = MembershipTrie::from_members(epoch, hashes);
        let digest = trie.root();
        let previous_close = rsnano_types::Blake2HashBuilder::new()
            .update(b"RAI-CLOSE")
            .update(epoch.to_le_bytes())
            .update(self.previous_close.as_bytes())
            .update(digest.as_bytes())
            .build();
        let mut announcement = self.template(self.announcement_seq + 1, BlockHash::ZERO, digest);
        announcement.kind = 8;
        announcement.members = trie.len() as u64;
        announcement.set_sketch(&trie.sketch().to_bytes());
        let announcements = keys
            .iter()
            .map(|key| {
                announcement.sign(key);
                announcement.clone()
            })
            .collect();
        *self = State::new(epoch + 1, weights, archive);
        self.previous_close = previous_close;
        self.closed = Some(ClosedView {
            trie,
            announcements,
        });
        self.receipts = receipts;
        self.local_receipts = local_receipts;
    }

    fn archive_acknowledged(&self, epoch: u64, digest: BlockHash) -> bool {
        self.weights
            .iter()
            .filter(|(_, w)| !w.is_zero())
            .all(|(rep, _)| self.receipts.get(&(epoch, *rep)) == Some(&digest))
    }

    fn template(&self, round: u64, parent: BlockHash, state: BlockHash) -> EpochClose {
        EpochClose {
            epoch: self.epoch,
            round,
            parent,
            previous_close: self.previous_close,
            state,
            kind: 0,
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            page: 0,
            pages: 0,
            hashes: vec![],
            base: BlockHash::ZERO,
            removed: vec![],
            members: self.trie.len() as u64,
            sketch: String::new(),
            certificate: vec![],
        }
    }
    /// Votes and receipts. Pages and requests have their own entry points.
    fn receive(&mut self, p: EpochClose) {
        if p.kind == 6 {
            if p.epoch <= self.epoch
                && (p.epoch >= self.epoch.saturating_sub(1)
                    || self.local_receipts.iter().any(|r| r.epoch == p.epoch))
                && !self.weights.weight(&p.voter).is_zero()
                && p.valid_receipt()
            {
                self.receipts.insert((p.epoch, p.voter), p.state);
            }
            return;
        }
        // Bounded future rounds are stored so a lagging replica can catch up at
        // tick speed once it holds the intermediate timeout certificates.
        if p.kind > 4
            || p.epoch != self.epoch
            || p.previous_close != self.previous_close
            || p.round > self.round + FUTURE_ROUNDS
            || self.weights.weight(&p.voter).is_zero()
            || !p.valid_vote()
        {
            return;
        }
        let accepted =
            self.rounds
                .entry(p.round)
                .or_default()
                .receive(p.clone(), &self.weights, self.total);
        if accepted && p.kind <= 2 && !p.state.is_zero() {
            self.candidates
                .entry(p.candidate_id())
                .or_insert_with(|| Candidate {
                    header: p.clone(),
                    sources: Vec::new(),
                    proposal: None,
                });
        }
    }

    fn certified(&self, id: BlockHash, k: VoteKind) -> bool {
        self.candidates.get(&id).is_some_and(|c| {
            self.rounds
                .get(&c.header.round)
                .is_some_and(|r| r.certificate(id, k))
        })
    }
    fn has_finalized_close(&self) -> bool {
        self.candidates.keys().any(|id| {
            self.well_formed(*id)
                && (self.certified(*id, VoteKind::First)
                    || (self.certified(*id, VoteKind::Notarize)
                        && self.certified(*id, VoteKind::Final)))
        })
    }
    /// Every round in `rounds` has a timeout certificate.
    fn timed_out(&self, rounds: std::ops::Range<u64>) -> bool {
        rounds.into_iter().all(|r| {
            self.rounds
                .get(&r)
                .is_some_and(|x| x.certificate(BlockHash::ZERO, VoteKind::Timeout))
        })
    }
    /// ParentOK: the candidate extends the genesis position with every earlier
    /// round timed out, or a notarized candidate of an earlier round with a
    /// timeout certificate for every round skipped between them. The parent's
    /// own round needs no timeout, and its ancestry is not re-validated: its
    /// notarization certificate has a correct signer that checked it.
    fn well_formed(&self, id: BlockHash) -> bool {
        let Some(c) = self.candidates.get(&id) else {
            return false;
        };
        let parent = c.header.parent;
        let from = if parent.is_zero() {
            0
        } else {
            let Some(p) = self.candidates.get(&parent) else {
                return false;
            };
            if p.header.round >= c.header.round || !self.certified(parent, VoteKind::Notarize) {
                return false;
            }
            p.header.round + 1
        };
        self.timed_out(from..c.header.round)
    }
    /// A candidate carries a certificate that decides the epoch.
    fn finalized(&self, id: BlockHash) -> bool {
        self.well_formed(id)
            && (self.certified(id, VoteKind::First)
                || (self.certified(id, VoteKind::Notarize) && self.certified(id, VoteKind::Final)))
    }
    /// The close decision: the earliest candidate on the chain of an explicitly
    /// finalized one. A notarized ancestor is finalized implicitly by its
    /// descendant, whatever else its own round certified; explicit finality
    /// excludes a timeout in its round, so every later candidate descends from
    /// it and all replicas reach the same ancestor. `None` until the whole
    /// chain is certified here.
    fn decision(&self) -> Option<BlockHash> {
        let mut id = *self.candidates.keys().find(|id| self.finalized(**id))?;
        loop {
            let parent = self.candidates[&id].header.parent;
            if parent.is_zero() {
                return Some(id);
            }
            if !self.certified(parent, VoteKind::Notarize) {
                return None;
            }
            id = parent;
        }
    }
    /// Whether every member of the announced view named by `state` is held
    /// here: its sketch decoded against the live membership with nothing on
    /// the announcer's side only, and the live membership without the members
    /// it omits has that root. Omitting them is the announcing quorum's D4
    /// judgment, not this replica's: a member it never saw cannot be finalized
    /// once that quorum closed without it.
    fn holds_members(&mut self, state: BlockHash) -> bool {
        let root = self.root();
        let Some(view) = self.views.get_mut(&state) else {
            return false;
        };
        if let Some((checked_root, result)) = view.subset {
            if checked_root == root {
                return result;
            }
        }
        let result = view.decoded(root) && view.only_theirs.is_empty() && {
            let excluded: HashSet<_> = view.only_mine.iter().copied().collect();
            self.trie.without(&excluded).root() == state
        };
        let view = self.views.get_mut(&state).unwrap();
        view.subset = Some((root, result));
        result
    }
    /// C2(a)-(e) for a candidate: the round assembler proposed it, its target
    /// is certified by announcement weight in its round, ParentOK holds and
    /// every member of the target is held here, so its entries verify
    /// locally; (c) held when the proposal was stored. Such a candidate takes
    /// a FIRST vote or a second-look share, and is complete once notarized.
    fn complete(&mut self, id: BlockHash) -> bool {
        let root = self.root();
        let Some(c) = self.candidates.get(&id) else {
            return false;
        };
        let (round, state) = (c.header.round, c.header.state);
        c.proposal.is_some()
            && self.certified_target(round, state)
            && self.well_formed(id)
            && (state == root || self.holds_members(state))
    }
    fn sign(&mut self, mut p: EpochClose, key: &PrivateKey, k: u8, out: &mut Vec<EpochClose>) {
        p.kind = k;
        p.pages = 0;
        p.page = 0;
        p.hashes.clear();
        p.sketch.clear();
        p.certificate.clear();
        p.base = BlockHash::ZERO;
        p.removed.clear();
        if matches!(k, 3 | 4) {
            p.parent = BlockHash::ZERO;
            p.state = BlockHash::ZERO;
        }
        p.sign(key);
        debug_trace(
            || serde_json::json!({"type":"close_vote","epoch":p.epoch,"round":p.round,"id":p.candidate_id(),"parent":p.parent,"state":p.state,"kind":k,"voter":p.voter}),
        );
        self.receive(p.clone());
        out.push(p);
    }
    /// Votes never include members, sketches or pages.
    fn packets(&self) -> Vec<EpochClose> {
        self.rounds
            .values()
            .flat_map(|r| r.votes.values().cloned())
            .collect()
    }

    fn drive(&mut self, keys: &[PrivateKey]) -> (Vec<EpochClose>, Option<Vec<BlockHash>>) {
        let mut out = Vec::new();
        let now = Instant::now();
        if self.ready && self.ready_since.is_none() {
            self.ready_since = Some(now);
        }
        // The decided candidate closes the epoch: with the live membership when
        // it is the proposed one, otherwise once its view is reconstructed.
        let root = self.root();
        if let Some(id) = self.decision() {
            let state = self.candidates[&id].header.state;
            if state == root {
                return (out, Some(self.trie.members().copied().collect()));
            }
            if let Some(members) = self.assemble_view(state) {
                return (out, Some(members));
            }
        }
        // Readiness gates announcing only; every draining replica votes.
        if !self.draining || self.has_finalized_close() {
            return (out, None);
        }
        self.rounds.entry(self.round).or_default();
        self.announce(keys, &mut out);
        if self.commit_due(now) {
            self.commit(keys, &mut out);
        }
        // Round timeouts keep the rounds rotating even while no target has
        // certificate support or the assembler stays silent; reconciliation
        // continues meanwhile.
        if !self.round_timer_armed && (self.enabled().is_some() || self.agreement_expired(now)) {
            self.arm_round_timer();
        }
        self.propose(keys, &mut out);
        let timed = self.round_timed_out();
        let current: Vec<_> = self
            .candidates
            .iter()
            .filter(|(_, c)| c.header.round == self.round)
            .map(|(id, c)| (*id, c.header.clone()))
            .collect();
        // The FIRST vote goes to the first proposal of the round whose checks
        // C2(a)-(e) complete before the deadline, whether or not its target is
        // this replica's own root: a replica holding more members votes it as
        // well. Past the deadline the vote is a timeout below. With a correct
        // assembler there is one proposal, so a Byzantine member cannot split
        // the correct first votes.
        if !timed {
            for (id, p) in &current {
                if !self.complete(*id) {
                    continue;
                }
                for key in keys {
                    let signer = self
                        .rounds
                        .get_mut(&self.round)
                        .unwrap()
                        .signers
                        .entry(key.public_key())
                        .or_default();
                    if signer.first.is_none() {
                        signer.first = Some(*id);
                        signer.notarized.insert(*id);
                        debug_trace(
                            || serde_json::json!({"type":"first_vote","epoch":self.epoch,"round":self.round,"id":id,"parent":p.parent,"state":p.state,"own":p.state == root,"rep":key.public_key()}),
                        );
                        self.sign(p.clone(), key, 0, &mut out);
                    }
                }
            }
        }
        for (id, p) in &current {
            // A second-look share re-runs C2(a)-(e) (C3). A FINAL vote is
            // bound to the FIRST vote only (FV): the authorization a quorum
            // gave that value is not withdrawn by a member this replica
            // learned since.
            let signable = self.complete(*id);
            for key in keys {
                let r = self.rounds.get_mut(&self.round).unwrap();
                let second = r.tally.second_look(id);
                let notarized = r.certificate(*id, VoteKind::Notarize);
                let signer = r.signers.entry(key.public_key()).or_default();
                let mut action = None;
                if second
                    && signable
                    && signer.first.is_some()
                    && !signer.notarized.contains(id)
                    && signer.final_vote.is_none()
                    && signer.notarized.len() < 3
                {
                    signer.notarized.insert(*id);
                    action = Some(1);
                } else if notarized
                    && signer.first == Some(*id)
                    && signer.final_vote.is_none()
                    && !signer.timeout
                    && signer.notarized.iter().all(|h| h == id)
                {
                    signer.final_vote = Some(*id);
                    action = Some(2);
                }
                if let Some(k) = action {
                    self.sign(p.clone(), key, k, &mut out);
                }
            }
        }
        // A complete candidate, notarized with its checks passed here, exits
        // the round without a timeout certificate (X1): the next round extends
        // it as parent and a finalized descendant finalizes it implicitly.
        // Otherwise a notarized value short of its FINAL quorum, with too few
        // FIRST timeouts to trigger timeout shares, would stall the epoch for
        // good. No share follows the exit.
        if current
            .iter()
            .any(|(id, _)| self.certified(*id, VoteKind::Notarize) && self.complete(*id))
        {
            debug_trace(
                || serde_json::json!({"type":"round_complete","epoch":self.epoch,"round":self.round}),
            );
            self.enter_round(self.round + 1);
            return (out, None);
        }
        // Otherwise a new close round requires a timeout certificate.
        let p = self.template(self.round, BlockHash::ZERO, BlockHash::ZERO);
        let timed = self.round_timed_out();
        for key in keys {
            let r = self.rounds.get_mut(&self.round).unwrap();
            let eligible = r.tally.should_timeout();
            let signer = r.signers.entry(key.public_key()).or_default();
            if signer.first.is_none() && timed {
                signer.first = Some(BlockHash::ZERO);
                signer.timeout = true;
                self.sign(p.clone(), key, 3, &mut out);
            } else if signer.first.is_some()
                && eligible
                && !signer.timeout
                && signer.final_vote.is_none()
            {
                signer.timeout = true;
                self.sign(p.clone(), key, 4, &mut out);
            }
        }
        if self.rounds[&self.round].certificate(BlockHash::ZERO, VoteKind::Timeout) {
            // Certified ancestors and fetched views are retained across rounds.
            self.enter_round(self.round + 1);
        }
        (out, None)
    }
}

pub(crate) struct EpochCloser {
    ledger: Arc<Ledger>,
    generators: Arc<VoteGenerators>,
    aec: Arc<AecService>,
    reps: Arc<Mutex<WalletRepresentatives>>,
    flooder: Mutex<MessageFlooder>,
    incoming: Mutex<VecDeque<(EpochClose, ChannelId)>>,
    state: Mutex<State>,
    epoch_start: Mutex<Option<Instant>>,
    termination_baseline: std::sync::atomic::AtomicU64,
    epoch_start_file: Option<std::path::PathBuf>,
}
impl EpochCloser {
    pub fn new(
        ledger: Arc<Ledger>,
        generators: Arc<VoteGenerators>,
        aec: Arc<AecService>,
        reps: Arc<Mutex<WalletRepresentatives>>,
        flooder: MessageFlooder,
    ) -> Self {
        let mut state = State::new(
            ledger.closed_epoch_count.load(Ordering::Acquire),
            ledger.rep_weights.read().clone(),
            vec![],
        );
        state.previous_close = state
            .epoch
            .checked_sub(1)
            .and_then(|e| {
                ledger
                    .store
                    .consensus_epochs
                    .close_id(&ledger.store.begin_read(), e)
            })
            .unwrap_or_default();
        let epoch_start_file =
            std::env::var_os("NANOSPAM_RAI_EPOCH_START_FILE").map(std::path::PathBuf::from);
        Self {
            epoch_start: Mutex::new(epoch_start_file.is_none().then(Instant::now)),
            termination_baseline: std::sync::atomic::AtomicU64::new(0),
            epoch_start_file,
            ledger,
            generators,
            aec,
            reps,
            flooder: Mutex::new(flooder),
            incoming: Default::default(),
            state: Mutex::new(state),
        }
    }
    pub fn receive(&self, message: EpochClose, channel: ChannelId) {
        let mut q = self.incoming.lock().unwrap();
        if q.len() < 4096 {
            q.push_back((message, channel));
        } else {
            DROPPED_CLOSE_PACKETS.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn epoch_deadline_reached(&self, epoch: u64) -> bool {
        if self.epoch_start_file.is_some()
            && std::env::var("NANOSPAM_RAI_CLOSED_EPOCHS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|limit| epoch >= limit)
        {
            return false;
        }
        let mut start = self.epoch_start.lock().unwrap();
        if start.is_none() {
            let Some(path) = &self.epoch_start_file else {
                return false;
            };
            let Ok(text) = std::fs::read_to_string(path) else {
                return false;
            };
            let Ok(millis) = text.trim().parse::<u64>() else {
                return false;
            };
            self.termination_baseline
                .store(self.aec.terminated_election_count(), Ordering::Relaxed);
            let wall_start = UNIX_EPOCH + Duration::from_millis(millis);
            let now = Instant::now();
            let wall_now = SystemTime::now();
            *start = Some(match wall_start.duration_since(wall_now) {
                Ok(delay) => now + delay,
                Err(_) => now - wall_now.duration_since(wall_start).unwrap(),
            });
            eprintln!(
                "EPOCH_SCHEDULE {}",
                serde_json::json!({"start_unix_ms":millis,"duration_seconds":self.ledger.epoch_length.load(Ordering::Relaxed)})
            );
        }
        let count = self
            .ledger
            .epoch_terminated_elections
            .load(Ordering::Relaxed);
        if count > 0 {
            let terminated = self
                .aec
                .terminated_election_count()
                .saturating_sub(self.termination_baseline.load(Ordering::Relaxed));
            return start.is_some_and(|s| Instant::now() >= s)
                && terminated >= count.saturating_mul(epoch.saturating_add(1));
        }
        let seconds = self.ledger.epoch_length.load(Ordering::Relaxed);
        start.is_some_and(|s| epoch_due(s, Instant::now(), epoch, seconds))
    }

    fn tick(&self) {
        if !self.ledger.epochs_enabled() {
            return;
        }
        let tick_started = Instant::now();
        let mut state = self.state.lock().unwrap();
        let incoming = std::mem::take(&mut *self.incoming.lock().unwrap());
        let incoming_count = incoming.len();
        let report_transport = state.last_send.elapsed() >= RETRANSMIT
            && std::env::var_os("RAI_CLOSE_VALIDATION_DIAGNOSTICS").is_some();
        let mut keys = Vec::new();
        self.reps.lock().unwrap().rep_priv_keys(&mut keys);
        let signing_key = keys
            .iter()
            .find(|k| !state.weights.weight(&k.public_key()).is_zero())
            .cloned();
        // Members named by peers are solicited by hash; a root known locally lets
        // the peer skip the block, a zero root makes it publish the block too.
        let root_of = |hash: &BlockHash| {
            self.ledger
                .any()
                .get_block(hash)
                .map(|block| block.root())
                .or_else(|| {
                    self.aec
                        .election_for_block(hash)
                        .map(|election| election.qualified_root().root)
                })
                .unwrap_or_else(|| Root::from(0u64))
        };
        let epoch = state.epoch;
        let missing_members = |hashes: &[BlockHash]| {
            self.ledger
                .epoch_close_missing_members(epoch, hashes)
                .into_iter()
                .map(|hash| (hash, root_of(&hash)))
                .collect::<Vec<_>>()
        };
        for (p, channel) in incoming {
            match p.kind {
                5 => state.receive_digest_page(p),
                7 => state.receive_leaf_page(p, missing_members),
                8 => state.receive_announcement(p, channel),
                10 => state.receive_commitment(p),
                11 => state.receive_proposal(p),
                9 => {
                    if let Some(key) = &signing_key {
                        state.serve_request(p, channel, key);
                    }
                }
                _ => {
                    let id = p.candidate_id();
                    let source =
                        p.kind <= 2 && p.valid_vote() && !state.weights.weight(&p.voter).is_zero();
                    state.receive(p);
                    if source {
                        if let Some(candidate) = state.candidates.get_mut(&id) {
                            if !candidate.sources.contains(&channel) && candidate.sources.len() < 16
                            {
                                candidate.sources.push(channel);
                            }
                        }
                    }
                }
            }
        }
        state.active_views();
        state.decode_sketches(missing_members);
        let mut drain_requests: Vec<(BlockHash, Root)> = Vec::new();
        if self.epoch_deadline_reached(state.epoch) {
            self.ledger.begin_epoch_drain();
            if !state.draining {
                let count = self
                    .ledger
                    .epoch_terminated_elections
                    .load(Ordering::Relaxed);
                if count > 0 {
                    let drain_unix_us = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_micros();
                    eprintln!(
                        "EPOCH_COUNT_REACHED {}",
                        serde_json::json!({
                            "epoch":state.epoch,"pid":std::process::id(),"unix_us":drain_unix_us,"length":count,
                            "threshold":count.saturating_mul(state.epoch.saturating_add(1)),
                            "terminated":self.aec.terminated_election_count().saturating_sub(self.termination_baseline.load(Ordering::Relaxed))
                        })
                    );
                }
                state.local_first = Arc::new(self.generators.begin_drain(state.epoch));
                state.draining = true;
                state.enter_round(0);
                debug_trace(
                    || serde_json::json!({"type":"drain_start","epoch":state.epoch,"local_first":state.local_first.len()}),
                );
            }
        }
        // D3 is live, not a sticky flag: newly visible elections also block signing.
        if state.has_finalized_close() {
            self.generators.seal_epoch(state.epoch);
        }
        // Readiness only matters while draining, so the container is scanned
        // only then; before that the tick must not stall vote application.
        let mut pending_targets = Vec::new();
        let (mut outgoing, closed) = if state.draining {
            let added = state.sync_membership(&self.ledger);
            if state.ready && !added.is_empty() {
                let root = state.root();
                eprintln!(
                    "EPOCH_MEMBERS_CHANGED {}",
                    serde_json::json!({
                        "pid":std::process::id(),"epoch":state.epoch,"round":state.round,"count":state.trie.len(),"state":root,
                        "added":added.iter().take(8).collect::<Vec<_>>(),"added_total":added.len()
                    })
                );
                debug_trace(
                    || serde_json::json!({"type":"members_changed","epoch":state.epoch,"round":state.round,"count":state.trie.len(),"state":root,"added":added}),
                );
            }
            let local_first = state.local_first.clone();
            self.aec
                .with_close_readiness(state.epoch, &local_first, |ready, pending| {
                    pending_targets = pending;
                    state.ready = ready;
                    state.drive(&keys)
                })
        } else {
            state.ready = false;
            state.drive(&keys)
        };
        let cycle = state.last_send.elapsed() >= RETRANSMIT;
        if state.ready && !state.ready_traced {
            state.ready_traced = true;
            let root = state.root();
            debug_trace(|| {
                serde_json::json!({"type":"ready","epoch":state.epoch,"round":state.round,"members":state.trie.len(),"state":root,
                    "membership":state.trie.members().collect::<Vec<_>>()})
            });
        }
        // Solicit the elections the drain is waiting for, plus the local FIRST
        // obligations whose election is gone, at the retransmission cadence.
        let mut recovering = 0;
        if state.draining
            && closed.is_none()
            && state
                .last_timeout_solicitation
                .is_none_or(|last| last.elapsed() >= RETRANSMIT)
        {
            state.last_timeout_solicitation = Some(Instant::now());
            let now = Instant::now();
            let mut candidates = self
                .generators
                .first_recovery_targets(state.epoch, &self.aec);
            candidates.extend(pending_targets);
            let mut seen = HashSet::new();
            candidates.retain(|target| seen.insert(*target));
            let targets = state.next_solicitation_batch(&candidates, now);
            recovering = targets.len();
            if !targets.is_empty() {
                eprintln!(
                    "EPOCH_CLOSE_SOLICIT {}",
                    serde_json::json!({
                        "pid":std::process::id(),"epoch":state.epoch,"round":state.round,"targets":targets.len(),
                        "examples":targets.iter().take(3).map(|(hash, _)| hash).collect::<Vec<_>>()
                    })
                );
            }
            drain_requests.extend(targets);
        }
        if closed.is_none() {
            let targets = state.reconcile_batch(Instant::now(), |hashes| {
                self.ledger
                    .epoch_close_missing_members(epoch, hashes)
                    .into_iter()
                    .collect()
            });
            if !targets.is_empty() {
                eprintln!(
                    "EPOCH_CLOSE_RECONCILE {}",
                    serde_json::json!({
                        "pid":std::process::id(),"epoch":state.epoch,"round":state.round,"missing":targets.len(),
                        "examples":targets.iter().take(3).map(|(hash, _)| hash).collect::<Vec<_>>()
                    })
                );
                drain_requests.extend(targets);
            }
        }
        if state.ready && cycle {
            let enabled = state.enabled();
            let root = state.root();
            eprintln!(
                "EPOCH_CLOSE_PROGRESS {}",
                serde_json::json!({
                    "rep": keys.first().map(|k| k.public_key()), "epoch":state.epoch, "round":state.round,
                    "members":state.trie.len(),
                    "state":root,
                    "assembler":state.assembler(state.round),
                    "ready_for_ms":state.ready_since.map(|since| since.elapsed().as_millis()),
                    "enabled":enabled,
                    "announcements":state.announcements.iter().map(|(rep, (a, _))| serde_json::json!({"rep":rep,"members":a.members,"state":a.state})).collect::<Vec<_>>(),
                    "commitments":state.rounds.get(&state.round).map(|r| r.commitments.iter().map(|(rep, c)| serde_json::json!({"rep":rep,"state":c.state})).collect::<Vec<_>>()).unwrap_or_default(),
                    "proposals":state.candidates.values().filter_map(|c| c.proposal.as_ref()).map(|p| serde_json::json!({"round":p.round,"parent":p.parent,"state":p.state,"rep":p.voter})).collect::<Vec<_>>(),
                    "views":state.views.iter().map(|(root, v)| serde_json::json!({"state":root,"decoded":v.decoded_for.is_some(),"sketch_failed":v.sketch_failed,"only_mine":v.only_mine.len(),"only_theirs":v.only_theirs.len(),"level2":v.level2.len(),"leaves":v.leaves.len()})).collect::<Vec<_>>(),
                    "reconcile_targets":state.reconcile_targets.len(),
                    "round_timer_armed":state.round_timer_armed,
                    "recovering":recovering,
                    "candidates":state.candidates.iter().map(|(id,c)| serde_json::json!({"id":id,"round":c.header.round,"state":c.header.state,"members":c.header.members})).collect::<Vec<_>>(),
                    "votes":state.rounds.iter().map(|(r,v)| (*r,v.votes.len())).collect::<BTreeMap<_,_>>()
                })
            );
        }

        if let Some(hashes) = closed {
            self.generators.seal_epoch(state.epoch);
            if let Ok(discarded) = self.aec.close_epoch(&self.ledger, state.epoch, &hashes) {
                // Persistence and AEC cleanup are complete at this boundary.
                // Capture it before digest formatting or post-close housekeeping.
                let closed_unix_us = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_micros();
                let digest = Ledger::epoch_state_hash(state.epoch, &hashes);
                // The decided candidate's round: a replica may have exited it
                // on notarization before learning finality.
                let decided_round = state
                    .decision()
                    .map_or(state.round, |id| state.candidates[&id].header.round);
                eprintln!(
                    "EPOCH_CLOSED {}",
                    serde_json::json!({"pid":std::process::id(),"unix_us":closed_unix_us,"epoch":state.epoch,"hash":digest,"blocks":hashes.len(),"round":decided_round,"local_round":state.round,"discarded":discarded})
                );
                let epoch = state.epoch;
                let roots = self
                    .aec
                    .recovery_entries(
                        &hashes.iter().map(|h| (*h, Root::ZERO)).collect::<Vec<_>>(),
                        epoch,
                    )
                    .into_iter()
                    .filter(|e| e.block.is_some() && hashes.binary_search(&e.hash()).is_ok())
                    .map(|e| e.root)
                    .collect();
                self.generators.apply_close(epoch, roots);
                state.advance(hashes, self.ledger.rep_weights.read().clone(), &keys);
                for key in &keys {
                    let mut receipt = state.template(0, BlockHash::ZERO, digest);
                    receipt.epoch = epoch;
                    receipt.kind = 6;
                    receipt.members = 0;
                    receipt.sign(key);
                    state.receive(receipt.clone());
                    state.local_receipts.push(receipt);
                }
                // Vote-generator cleanup, state advancement, and local receipt
                // construction are complete; ordinary retransmission follows.
                let complete_unix_us = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_micros();
                eprintln!(
                    "EPOCH_CLOSE_COMPLETE {}",
                    serde_json::json!({"pid":std::process::id(),"epoch":epoch,"unix_us":complete_unix_us})
                );
            }
        }
        if let Some(receipt) = state.local_receipts.first() {
            if state
                .local_receipts
                .iter()
                .all(|r| state.archive_acknowledged(r.epoch, r.state))
                && (!state.archive.is_empty() || state.closed.is_some())
            {
                debug_trace(
                    || serde_json::json!({"type":"archive_acknowledged","epoch":receipt.epoch,"state":receipt.state,"packets":state.archive.len()}),
                );
                state.archive.clear();
                state.closed = None;
            }
        }
        // Only compact votes, receipts and announcements are flooded. Pages go
        // to the peer that asked and are never broadcast.
        if cycle {
            if let Some(closed) = &state.closed {
                outgoing.extend(closed.announcements.iter().cloned());
            }
            state.queue_retransmission();
            state.last_send = Instant::now();
        }
        outgoing.extend(state.retransmission_burst());
        let mut targeted = if let Some(key) = &signing_key {
            state.page_requests(key, Instant::now())
        } else {
            Vec::new()
        };
        for _ in 0..PAGE_BURST {
            if let Some(reply) = state.pages_out.pop_front() {
                targeted.push(reply);
            } else {
                break;
            }
        }
        let drain_epoch = state.epoch;
        drop(state);
        let outgoing_count = outgoing.len();
        let processing_ms = tick_started.elapsed().as_millis();
        let mut flooder = self.flooder.lock().unwrap();
        for chunk in drain_requests.chunks(rsnano_messages::ConfirmReq::HASHES_MAX) {
            flooder.flood_prs_and_some_non_prs(
                &Message::ConfirmReq(
                    rsnano_messages::ConfirmReq::new(chunk.to_vec()).with_epoch(drain_epoch),
                ),
                TrafficType::ConfirmationRequests,
                1.0,
            );
        }
        for packet in outgoing {
            flooder.flood_prs_and_some_non_prs(
                &Message::EpochClose(packet),
                TrafficType::EpochClose,
                1.0,
            );
        }
        for (channel, packet) in targeted {
            flooder.try_send_channel_id(
                channel,
                &Message::EpochClose(packet),
                TrafficType::EpochClose,
            );
        }
        if report_transport {
            eprintln!(
                "EPOCH_CLOSE_TRANSPORT {}",
                serde_json::json!({
                    "pid":std::process::id(),"incoming":incoming_count,"outgoing":outgoing_count,
                    "processing_ms":processing_ms,"total_ms":tick_started.elapsed().as_millis(),
                    "dropped":DROPPED_CLOSE_PACKETS.load(Ordering::Relaxed)
                })
            );
        }
    }
}
pub(crate) struct EpochCloseTicker(pub Arc<EpochCloser>);
impl Tickable for EpochCloseTicker {
    fn tick(&mut self, _: &CancellationToken) {
        self.0.tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_round_assembler_closes_with_one_silent_member() {
        let ledger = Ledger::new_null();
        let mut replicas: Vec<_> = (0..6).map(|_| ready_state(&ledger)).collect();
        let keys = sorted_keys();
        let mut decisions = vec![None; 5];
        for step in 0..12 {
            let mut packets = Vec::new();
            for i in 0..5 {
                let (out, decision) = replicas[i].drive(&[keys[i].clone()]);
                if decision.is_some() {
                    decisions[i] = decision;
                }
                packets.extend(out);
            }
            exchange(&mut replicas, packets);
            if step == 0 {
                assert!(
                    replicas.iter().all(|r| r.candidates.is_empty()),
                    "no proposal while a representative has not announced"
                );
                for replica in &mut replicas {
                    replica.ready_since = Some(Instant::now() - AGREEMENT_TIMEOUT);
                }
            }
        }
        assert!(decisions.iter().all(|d| d.as_ref() == Some(&vec![])));
        assert!(
            replicas[..5].iter().all(|r| r.round == 0),
            "the assembler's value certifies in round 0"
        );
        assert_eq!(replicas[0].candidates.len(), 1, "one proposal per round");
        let candidate = replicas[1].candidates.values().next().unwrap();
        assert_eq!(
            candidate.proposal.as_ref().map(|p| p.voter),
            Some(keys[0].public_key()),
            "the round-0 assembler is the first committee member"
        );
        assert!(
            candidate.proposal.as_ref().unwrap().certificate.len() >= 4,
            "the proposal carries the announcements certifying its target"
        );
    }

    #[test]
    fn nothing_is_proposed_until_every_representative_announces_the_same_root() {
        let ledgers: Vec<_> = (0..6).map(|_| Ledger::new_null()).collect();
        let extra: rsnano_types::Block = rsnano_types::SavedBlock::new_test_instance().into();
        ledgers[3].record_epoch_block(0, extra.clone());
        let mut replicas: Vec<_> = ledgers.iter().map(ready_state).collect();
        let keys = sorted_keys();
        let round = |replicas: &mut Vec<State>| {
            let mut packets = Vec::new();
            let mut decisions = Vec::new();
            for i in 0..6 {
                replicas[i].sync_membership(&ledgers[i]);
                let (out, decision) = replicas[i].drive(&[keys[i].clone()]);
                decisions.push(decision);
                packets.extend(out);
            }
            exchange(replicas, packets);
            decisions
        };
        for _ in 0..3 {
            round(&mut replicas);
        }
        assert!(
            replicas.iter().all(|r| r.candidates.is_empty()),
            "one replica holds an extra member, so nobody proposes"
        );
        assert!(replicas.iter().all(|r| r.announcements.len() == 6));
        assert!(!replicas[0].round_timer_armed);
        assert_eq!(
            (
                replicas[0].active_views().len(),
                replicas[3].active_views().len()
            ),
            (1, 1),
            "each side tracks the other membership as a view"
        );
        // The others learn the member (as if their solicitations were answered).
        for ledger in &ledgers {
            ledger.record_epoch_block(0, extra.clone());
        }
        let mut decisions = vec![None; 6];
        // Announce, commit, propose, vote, finalize: one exchange each.
        for _ in 0..5 {
            for (i, decision) in round(&mut replicas).into_iter().enumerate() {
                if decision.is_some() {
                    decisions[i] = decision;
                }
            }
        }
        assert!(
            decisions
                .iter()
                .all(|d| d.as_ref() == Some(&vec![extra.hash()])),
            "every replica closes on the reconciled membership"
        );
        assert!(replicas.iter().all(|r| r.round == 0), "closed in round 0");
        assert!(replicas.iter().all(|r| r.round_timer_armed));
        for replica in &mut replicas {
            replica.active_views();
            assert!(replica.views.is_empty());
        }
    }

    #[test]
    fn round_zero_is_timed_from_the_proposal_not_from_the_drain() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        s.enter_round(0);
        let key = PrivateKey::from(1);
        s.rounds.get_mut(&0).unwrap().started = Instant::now() - round_timeout(0) * 10;
        let (out, _) = s.drive(&[key.clone()]);
        assert!(
            out.iter().all(|p| p.kind == 8),
            "only the announcement goes out while the vote is not due"
        );
        for i in 1..=6 {
            let mut announcement = out[0].clone();
            announcement.sign(&PrivateKey::from(i));
            s.receive_announcement(announcement, ChannelId::from(i as usize));
        }
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![10],
            "every sketch names our pair, so the round-0 commitment is signed"
        );
        assert!(!s.round_timer_armed, "one commitment certifies nothing");
        let root = s.root();
        commit(&mut s, 2..=6, root);
        let (out, _) = s.drive(&[key.clone()]);
        assert!(
            s.round_timer_armed,
            "the timer starts when the target is certified"
        );
        assert!(!s.round_timed_out(), "and not from the stale drain start");
        assert!(
            out.is_empty(),
            "nothing is voted until the assembler proposes"
        );
        let proposal = propose(&mut s, BlockHash::ZERO, root);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![0],
            "the replica votes FIRST for the assembler's value"
        );
        assert_eq!(out[0].candidate_id(), proposal.candidate_id());
        assert_eq!(out[0].state, s.root());
        s.rounds.get_mut(&0).unwrap().started = Instant::now() - round_timeout(0);
        assert!(s.round_timed_out());
        let late = s.drive(&[key]).0;
        assert_eq!(
            late.iter().map(|p| p.kind).collect::<Vec<_>>(),
            Vec::<u8>::new(),
            "one FIRST vote per round"
        );
    }

    #[test]
    fn the_assembler_rotates_through_the_committee_by_round_and_epoch() {
        let mut s = state();
        let order = [4, 3, 5, 6, 2, 1].map(|i| PrivateKey::from(i).public_key());
        for round in 0..12u64 {
            assert_eq!(
                s.assembler(round),
                Some(order[round as usize % 6]),
                "round {round}"
            );
        }
        s.epoch = 1;
        assert_eq!(s.assembler(0), Some(order[1]), "the epoch shifts the start");
        assert_eq!(s.assembler(5), Some(order[0]));
        let mut empty = State::new(0, RepWeights::default(), vec![]);
        assert_eq!(empty.assembler(0), None);
        assert!(empty.enabled().is_none());
    }

    #[test]
    fn the_assembler_proposes_once_per_round_with_the_certifying_announcements() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = assembler_key(&s);
        s.ready_since = Some(Instant::now() - AGREEMENT_TIMEOUT);
        let root = s.root();
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![8, 10],
            "no proposal while the target is uncertified"
        );
        commit(&mut s, 1..=6, root);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![11, 0],
            "the assembler proposes and votes its own value"
        );
        let proposal = &out[0];
        assert!(proposal.valid_proposal());
        assert_eq!(proposal.voter, key.public_key());
        assert_eq!((proposal.parent, proposal.state), (BlockHash::ZERO, root));
        assert!(proposal.certificate.len() >= 4);
        assert!(
            proposal
                .certificate
                .iter()
                .all(|signer| proposal.commitment_of(signer).valid_commitment())
        );
        assert_eq!(out[1].candidate_id(), proposal.candidate_id());
        assert!(
            s.drive(&[key]).0.is_empty(),
            "one proposal and one FIRST vote per round"
        );
    }

    #[test]
    fn a_proposal_needs_the_round_assembler_and_a_certified_target() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        let mut forged = s.template(0, BlockHash::ZERO, root);
        forged.kind = 11;
        forged.sign(&PrivateKey::from(1));
        s.receive_proposal(forged);
        assert!(
            s.candidates.is_empty(),
            "key 1 is not the round-0 assembler"
        );
        let bare = propose(&mut s, BlockHash::ZERO, root);
        let id = bare.candidate_id();
        assert!(s.candidates[&id].proposal.is_some());
        assert!(!s.complete(id), "no announcement certifies the target");
        assert!(
            s.drive(&[PrivateKey::from(1)])
                .0
                .iter()
                .all(|p| p.kind == 8)
        );
        // The proposal itself carries the announcements a voter missed.
        let mut other = state();
        other.sync_membership(&ledger);
        commit(&mut other, 2..=5, root);
        let carried = propose(&mut other, BlockHash::ZERO, root);
        assert_eq!(carried.certificate.len(), 4);
        s.receive_proposal(carried);
        assert_eq!(s.rounds[&0].commitments.len(), 4);
        assert!(s.complete(id));
        let (out, _) = s.drive(&[PrivateKey::from(1)]);
        assert!(out.iter().any(|p| p.kind == 0 && p.candidate_id() == id));
        // A proposal whose round is another assembler's is ignored as well.
        let mut wrong_round = s.template(1, BlockHash::ZERO, root);
        wrong_round.kind = 11;
        wrong_round.sign(&assembler_key(&s));
        s.receive_proposal(wrong_round);
        assert!(s.candidates.values().all(|c| c.header.round == 0));
    }

    #[test]
    fn an_equivocating_assembler_stores_a_bounded_number_of_proposals() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        commit(&mut s, 1..=6, root);
        for parent in 1..=(PROPOSALS_PER_ROUND as u64 + 4) {
            propose(&mut s, parent.into(), root);
        }
        assert_eq!(s.candidates.len(), PROPOSALS_PER_ROUND);
        // A value voted into existence can still get its proposal attached.
        let mut vote = s.template(0, 99.into(), root);
        vote.sign(&PrivateKey::from(2));
        let id = vote.candidate_id();
        s.receive(vote);
        assert!(s.candidates[&id].proposal.is_none());
        propose(&mut s, 99.into(), root);
        assert!(s.candidates[&id].proposal.is_some());
        s.queue_retransmission();
        assert_eq!(
            s.retransmit.iter().filter(|p| p.kind == 11).count(),
            PROPOSALS_PER_ROUND + 1,
            "held proposals are rebroadcast with the close history"
        );
    }

    #[test]
    fn a_silent_assembler_costs_one_round_and_the_next_assembler_closes() {
        let ledger = Ledger::new_null();
        let mut replicas: Vec<_> = (0..6).map(|_| ready_state(&ledger)).collect();
        let keys = sorted_keys();
        for replica in &mut replicas {
            replica.ready_since = Some(Instant::now() - AGREEMENT_TIMEOUT);
        }
        assert_eq!(replicas[1].assembler(0), Some(keys[0].public_key()));
        let mut decisions = vec![None; 6];
        for _ in 0..16 {
            let mut packets = Vec::new();
            // The round-0 assembler (replica 0) never drives.
            for i in 1..6 {
                match replicas[i].round {
                    0 if replicas[i].round_timer_armed => {
                        replicas[i].rounds.get_mut(&0).unwrap().started =
                            Instant::now() - round_timeout(0);
                    }
                    1 => {
                        replicas[i].rounds.get_mut(&1).unwrap().started =
                            Instant::now() - round_timeout(1) / 2;
                    }
                    _ => {}
                }
                let (out, decision) = replicas[i].drive(&[keys[i].clone()]);
                if decision.is_some() {
                    decisions[i] = decision;
                }
                packets.extend(out);
            }
            exchange(&mut replicas, packets);
        }
        assert!(
            decisions[1..].iter().all(|d| d.as_ref() == Some(&vec![])),
            "every active replica closes: {decisions:?}"
        );
        let replica = &replicas[1];
        assert!(replica.rounds[&0].certificate(BlockHash::ZERO, VoteKind::Timeout));
        assert!(
            replica.candidates.values().all(|c| c.header.round == 1),
            "no value was proposed in round 0"
        );
        let decided = replica.decision().unwrap();
        assert_eq!(
            replica.candidates[&decided]
                .proposal
                .as_ref()
                .map(|p| p.voter),
            Some(keys[1].public_key()),
            "the round-1 assembler proposed the close"
        );
    }

    #[test]
    fn differing_views_are_decoded_from_the_announced_sketch() {
        let ledger_a = Ledger::new_null();
        let ledger_b = Ledger::new_null();
        let mut lattice = rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder::new();
        let shared = lattice.genesis().send(1, 1);
        let only_a = lattice.genesis().send(2, 1);
        let only_b = lattice.genesis().send(3, 1);
        for ledger in [&ledger_a, &ledger_b] {
            ledger.record_epoch_block(0, shared.clone());
        }
        ledger_a.record_epoch_block(0, only_a.clone());
        ledger_b.record_epoch_block(0, only_b.clone());
        let mut a = ready_state(&ledger_a);
        let mut b = ready_state(&ledger_b);
        let (key_a, key_b) = (PrivateKey::from(1), PrivateKey::from(2));
        let mut out = Vec::new();
        a.announce(&[key_a.clone()], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].members, 2);
        assert!(
            out[0].hashes.is_empty(),
            "no digests travel with the sketch"
        );
        b.receive_announcement(out[0].clone(), ChannelId::from(1));
        b.active_views();
        b.decode_sketches(|hashes| unknown_members(&ledger_b, hashes));
        let root_b = b.root();
        let view = &b.views[&a.root()];
        assert!(view.decoded(root_b));
        assert_eq!(view.only_theirs, vec![only_a.hash()]);
        assert_eq!(view.only_mine, vec![only_b.hash()]);
        assert_eq!(
            b.reconcile_targets.keys().copied().collect::<Vec<_>>(),
            vec![only_a.hash()],
            "the member A holds is solicited directly"
        );
        assert!(
            b.page_requests(&key_b, Instant::now()).is_empty(),
            "a decoded sketch needs no pages"
        );
        // Once the member is certified locally the difference shrinks to B's extra.
        ledger_b.record_epoch_block(0, only_a.clone());
        b.sync_membership(&ledger_b);
        b.decode_sketches(|hashes| unknown_members(&ledger_b, hashes));
        let root_b = b.root();
        let view = &b.views[&a.root()];
        assert!(view.decoded(root_b));
        assert!(view.only_theirs.is_empty());
        assert_eq!(view.only_mine, vec![only_b.hash()]);
        assert!(
            b.reconcile_batch(Instant::now(), |_| HashSet::new())
                .is_empty(),
            "a member learned since is no longer solicited"
        );
    }

    #[test]
    fn an_undecodable_sketch_falls_back_to_digest_pages() {
        let ledger_a = Ledger::new_null();
        let ledger_b = Ledger::new_null();
        let mut lattice = rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder::new();
        let shared = lattice.genesis().send(1, 1);
        let only_a = lattice.genesis().send(2, 1);
        for ledger in [&ledger_a, &ledger_b] {
            ledger.record_epoch_block(0, shared.clone());
        }
        ledger_a.record_epoch_block(0, only_a.clone());
        let mut a = ready_state(&ledger_a);
        let mut b = ready_state(&ledger_b);
        let (key_a, key_b) = (PrivateKey::from(1), PrivateKey::from(2));
        let mut out = Vec::new();
        a.announce(&[key_a.clone()], &mut out);
        b.receive_announcement(out[0].clone(), ChannelId::from(1));
        b.active_views();
        // A difference too large to decode (simulated) leaves the view on pages.
        b.views.get_mut(&a.root()).unwrap().sketch_failed = true;
        // B asks A for the level-1 digests first.
        let requests = b.page_requests(&key_b, Instant::now());
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1.pages, EpochClose::LEVEL1_PAGE);
        a.serve_request(requests[0].1.clone(), ChannelId::from(2), &key_a);
        let (_, level1) = a.pages_out.pop_front().unwrap();
        assert!(level1.valid_level1_page());
        b.receive_digest_page(level1);
        for view in b.views.values_mut() {
            view.requested.clear();
        }
        // Then the level-2 page of the bucket that differs.
        let requests = b.page_requests(&key_b, Instant::now());
        assert_eq!(requests.len(), 1);
        let (channel, request) = &requests[0];
        assert_eq!(*channel, ChannelId::from(1));
        assert_eq!(request.pages, EpochClose::LEVEL2_PAGE);
        assert_eq!(
            request.page,
            MembershipTrie::bucket_of(&only_a.hash()) as u16
        );
        assert!(
            b.page_requests(&key_b, Instant::now()).is_empty(),
            "a request is not repeated within the retransmission interval"
        );
        a.serve_request(request.clone(), ChannelId::from(2), &key_a);
        let (_, level2) = a.pages_out.pop_front().unwrap();
        assert_eq!(level2.kind, 5);
        b.receive_digest_page(level2);
        // Then the leaf, which names the member B lacks.
        let requests = b.page_requests(&key_b, Instant::now());
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1.pages, EpochClose::LEAF_PAGE);
        assert_eq!(
            requests[0].1.page,
            MembershipTrie::prefix_of(&only_a.hash())
        );
        a.serve_request(requests[0].1.clone(), ChannelId::from(2), &key_a);
        let (_, leaf) = a.pages_out.pop_front().unwrap();
        assert_eq!(leaf.kind, 7);
        assert_eq!(leaf.hashes, vec![only_a.hash()]);
        b.receive_leaf_page(leaf, |hashes| unknown_members(&ledger_b, hashes));
        assert_eq!(
            b.reconcile_targets.keys().copied().collect::<Vec<_>>(),
            vec![only_a.hash()]
        );
        assert!(
            b.page_requests(&key_b, Instant::now()).is_empty(),
            "nothing left to fetch"
        );
        // Once the member is certified locally the roots agree and the view goes.
        ledger_b.record_epoch_block(0, only_a.clone());
        b.sync_membership(&ledger_b);
        assert_eq!(b.root(), a.root());
        b.active_views();
        assert!(b.views.is_empty());
        assert!(
            b.reconcile_batch(Instant::now(), |_| HashSet::new())
                .is_empty(),
            "a member learned since is no longer solicited"
        );
        // A request for a view nobody holds is ignored.
        let mut stray = requests[0].1.clone();
        stray.state = 99.into();
        stray.sign(&key_b);
        a.serve_request(stray, ChannelId::from(2), &key_a);
        assert!(a.pages_out.is_empty());
    }

    #[test]
    fn a_finalized_close_this_replica_never_held_is_assembled_from_its_view() {
        let ledger_lagging = Ledger::new_null();
        let ledger_closed = Ledger::new_null();
        let mut lattice = rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder::new();
        let shared = lattice.genesis().send(1, 1);
        let missing = lattice.genesis().send(2, 1);
        let extra = lattice.genesis().send(3, 1);
        for ledger in [&ledger_lagging, &ledger_closed] {
            ledger.record_epoch_block(0, shared.clone());
        }
        ledger_closed.record_epoch_block(0, missing.clone());
        ledger_lagging.record_epoch_block(0, extra.clone());
        let mut lagging = ready_state(&ledger_lagging);
        let mut peer = ready_state(&ledger_closed);
        let closed_root = peer.root();
        let closed_members: Vec<_> = peer.trie.members().copied().collect();
        // The peers finalized the close with `closed_root` in round 0.
        let mut proposal = lagging.template(0, BlockHash::ZERO, closed_root);
        for i in 1..=6 {
            proposal.sign(&PrivateKey::from(i));
            lagging.receive(proposal.clone());
        }
        assert!(lagging.has_finalized_close());
        let (out, decision) = lagging.drive(&[]);
        assert!(out.is_empty());
        assert!(decision.is_none(), "the view is unknown so far");
        // The peer advanced and keeps announcing the closed membership.
        peer.advance(
            closed_members.clone(),
            peer.weights.clone(),
            &[PrivateKey::from(2)],
        );
        let announcement = peer.closed.as_ref().unwrap().announcements[0].clone();
        lagging.receive_announcement(announcement, ChannelId::from(2));
        let key = PrivateKey::from(1);
        lagging.active_views();
        lagging.decode_sketches(|hashes| unknown_members(&ledger_lagging, hashes));
        assert!(
            lagging.page_requests(&key, Instant::now()).is_empty(),
            "the closed sketch decodes, so no pages are fetched"
        );
        assert_eq!(
            lagging
                .reconcile_targets
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![missing.hash()]
        );
        assert!(
            lagging.drive(&[]).1.is_none(),
            "the missing member must be certified locally first"
        );
        ledger_lagging.record_epoch_block(0, missing.clone());
        lagging.sync_membership(&ledger_lagging);
        lagging.decode_sketches(|hashes| unknown_members(&ledger_lagging, hashes));
        let (_, decision) = lagging.drive(&[]);
        assert_eq!(decision, Some(closed_members));
        assert!(
            lagging.trie.contains(&extra.hash()),
            "the live membership keeps the member the close omitted"
        );
    }

    #[test]
    fn the_enabled_value_needs_certificate_weight_of_this_rounds_commitments() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        assert_eq!(s.enabled(), None);
        commit(&mut s, 2..=4, root);
        assert_eq!(
            s.enabled(),
            None,
            "three of six are below certificate weight"
        );
        let mut later = s.template(1, BlockHash::ZERO, root);
        later.kind = 10;
        later.members = 0;
        later.sign(&PrivateKey::from(5));
        s.receive_commitment(later);
        assert_eq!(
            s.enabled(),
            None,
            "a commitment of another round does not count"
        );
        let mut second = s.template(0, BlockHash::ZERO, 9.into());
        second.kind = 10;
        second.members = 0;
        second.sign(&PrivateKey::from(2));
        s.receive_commitment(second);
        commit(&mut s, 5..=5, root);
        assert_eq!(
            s.enabled(),
            Some(root),
            "a second root from one representative in the round is ignored"
        );
        assert!(s.certified_target(0, root));
        assert!(!s.certified_target(1, root));
        let mut out = Vec::new();
        s.commit(&[PrivateKey::from(1)], &mut out);
        assert_eq!(out.len(), 1);
        s.commit(&[PrivateKey::from(1)], &mut out);
        assert_eq!(out.len(), 1, "a replica commits once per round");
        assert_eq!(s.rounds[&0].commitments.len(), 5);
        s.enter_round(1);
        assert_eq!(
            s.enabled(),
            None,
            "the value of round 0 is not enabled in round 1"
        );
        assert_eq!(s.rounds[&1].commitments.len(), 1);
    }

    #[test]
    fn rounds_keep_rotating_while_no_membership_has_certificate_support() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        s.enter_round(0);
        let key = PrivateKey::from(1);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(out.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![8]);
        // Every other representative agrees on a different membership.
        let mut other = out[0].clone();
        other.state = 9.into();
        for i in 1..=6 {
            let signer = PrivateKey::from(i);
            if signer.public_key() == key.public_key() {
                continue;
            }
            other.sign(&signer);
            s.receive_announcement(other.clone(), ChannelId::from(i as usize));
        }
        s.ready_since = Some(Instant::now() - AGREEMENT_TIMEOUT);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![10],
            "the phase expired: our pair is committed, nothing is voted"
        );
        assert!(s.round_timer_armed, "and the round is timed from here");
        s.rounds.get_mut(&0).unwrap().started = Instant::now() - round_timeout(0);
        let (out, _) = s.drive(&[key]);
        assert_eq!(out.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn a_changed_membership_is_announced_and_committed_only_once_per_round() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = PrivateKey::from(1);
        let (out, _) = s.drive(&[key.clone()]);
        let announced = out[0].clone();
        for i in 1..=6 {
            let mut announcement = announced.clone();
            announcement.sign(&PrivateKey::from(i));
            s.receive_announcement(announcement, ChannelId::from(i as usize));
        }
        // A member arrives before the commitment is signed.
        ledger.record_epoch_block(0, rsnano_types::SavedBlock::new_test_instance().into());
        s.sync_membership(&ledger);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![8],
            "the new root is announced, nothing committed or voted"
        );
        assert!(s.candidates.is_empty());
        assert_eq!(out[0].members, 1);
        for i in 1..=6 {
            let mut announcement = out[0].clone();
            announcement.sign(&PrivateKey::from(i));
            s.receive_announcement(announcement, ChannelId::from(i as usize));
        }
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(out.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![10]);
        let committed = out[0].state;
        assert_eq!(committed, s.root());
        // Another member after the commitment: announced, not committed again.
        ledger.record_epoch_block(
            0,
            rsnano_types::SavedBlock::new_test_instance_with(
                rsnano_types::StateBlockArgs {
                    representative: 999.into(),
                    ..rsnano_types::StateBlockArgs::new_test_instance()
                }
                .into(),
            )
            .into(),
        );
        s.sync_membership(&ledger);
        s.ready_since = Some(Instant::now() - AGREEMENT_TIMEOUT);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![8],
            "a second pair is never signed in the same round"
        );
        assert_eq!(s.local_commitments.len(), 1);
        commit(&mut s, 2..=6, committed);
        propose(&mut s, BlockHash::ZERO, committed);
        s.active_views();
        s.decode_sketches(|hashes| unknown_members(&ledger, hashes));
        let (out, _) = s.drive(&[key]);
        assert_eq!(
            out.iter().map(|p| (p.kind, p.state)).collect::<Vec<_>>(),
            vec![(0, committed)],
            "the certified root is voted although the membership grew since"
        );
    }

    #[test]
    fn an_unready_replica_votes_the_enabled_value_but_announces_nothing() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = PrivateKey::from(1);
        let (out, _) = s.drive(&[key.clone()]);
        let announced = out[0].clone();
        // A late election opens after the announcement: D3 no longer holds.
        s.ready = false;
        commit(&mut s, 2..=6, announced.state);
        propose(&mut s, BlockHash::ZERO, announced.state);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![0],
            "the quorum carries the readiness judgment, so the vote is cast"
        );
        ledger.record_epoch_block(0, rsnano_types::SavedBlock::new_test_instance().into());
        s.sync_membership(&ledger);
        let (out, _) = s.drive(&[key]);
        assert!(
            out.is_empty(),
            "an unready replica announces no new root and never votes twice"
        );
    }

    #[test]
    fn a_candidate_nobody_announced_is_incomplete_once_the_membership_grows() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        commit(&mut s, 1..=6, root);
        let id = propose(&mut s, BlockHash::ZERO, root).candidate_id();
        assert!(s.complete(id));
        let block: rsnano_types::Block = rsnano_types::SavedBlock::new_test_instance().into();
        ledger.record_epoch_block(0, block);
        s.sync_membership(&ledger);
        assert!(
            !s.complete(id),
            "no announced view names the members the candidate omits"
        );
        assert!(
            s.drive(&[PrivateKey::from(1)])
                .0
                .iter()
                .all(|p| p.kind >= 3)
        );
    }

    #[test]
    fn a_quorum_enabled_subset_gets_this_replicas_first_vote() {
        let ledger_mine = Ledger::new_null();
        let ledger_theirs = Ledger::new_null();
        let mut lattice = rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder::new();
        let shared = lattice.genesis().send(1, 1);
        let extra = lattice.genesis().send(2, 1);
        for ledger in [&ledger_mine, &ledger_theirs] {
            ledger.record_epoch_block(0, shared.clone());
        }
        ledger_mine.record_epoch_block(0, extra.clone());
        let mut mine = ready_state(&ledger_mine);
        let mut theirs = ready_state(&ledger_theirs);
        let their_root = theirs.root();
        let mut out = Vec::new();
        theirs.announce(&[PrivateKey::from(2)], &mut out);
        for i in 2..=6 {
            let mut announcement = out[0].clone();
            announcement.sign(&PrivateKey::from(i));
            mine.receive_announcement(announcement, ChannelId::from(i as usize));
        }
        mine.active_views();
        mine.decode_sketches(|hashes| unknown_members(&ledger_mine, hashes));
        let (out, _) = mine.drive(&[PrivateKey::from(1)]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![8],
            "our own root is announced while the phase lasts"
        );
        assert_eq!(mine.enabled(), None, "sketches certify nothing");
        commit(&mut mine, 2..=6, their_root);
        assert_eq!(mine.enabled(), Some(their_root));
        propose(&mut mine, BlockHash::ZERO, their_root);
        mine.ready_since = Some(Instant::now() - AGREEMENT_TIMEOUT);
        let (out, _) = mine.drive(&[PrivateKey::from(1)]);
        assert_eq!(
            out.iter().map(|p| (p.kind, p.state)).collect::<Vec<_>>(),
            vec![(10, mine.root()), (0, their_root)],
            "our own root is announced, and the FIRST vote adopts the certified value \
             that omits a member held here"
        );
        // Finalized, the close is assembled without the omitted member.
        for i in 2..=6 {
            let mut vote = mine.template(0, BlockHash::ZERO, their_root);
            vote.sign(&PrivateKey::from(i));
            mine.receive(vote);
        }
        let (_, decision) = mine.drive(&[PrivateKey::from(1)]);
        assert_eq!(decision, Some(vec![shared.hash()]));
        // A view with members this replica lacks is never held.
        let mut lagging = ready_state(&ledger_theirs);
        let announcement = mine.local_announcements[0].clone();
        lagging.receive_announcement(announcement, ChannelId::from(1));
        lagging.active_views();
        lagging.decode_sketches(|hashes| unknown_members(&ledger_theirs, hashes));
        assert!(!lagging.holds_members(mine.root()));
    }

    #[test]
    fn a_member_learned_after_first_does_not_withdraw_the_final_vote() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = PrivateKey::from(1);
        let (out, _) = s.drive(&[key.clone()]);
        let announced = out[0].clone();
        commit(&mut s, 1..=6, announced.state);
        propose(&mut s, BlockHash::ZERO, announced.state);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(out.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![0]);
        let candidate = out[0].clone();
        ledger.record_epoch_block(0, rsnano_types::SavedBlock::new_test_instance().into());
        s.sync_membership(&ledger);
        assert!(!s.complete(candidate.candidate_id()));
        // Notarized by certificate weight, short of a fast certificate.
        for i in 2..=4 {
            let mut vote = candidate.clone();
            vote.sign(&PrivateKey::from(i));
            s.receive(vote);
        }
        let (out, _) = s.drive(&[key]);
        assert!(
            out.iter()
                .any(|p| p.kind == 2 && p.candidate_id() == candidate.candidate_id()),
            "the FINAL vote follows the FIRST vote, not the current membership"
        );
        assert!(
            out.iter().any(|p| p.kind == 8 && p.state == s.root()),
            "the grown membership is announced for a later round"
        );
    }

    #[test]
    fn a_child_candidate_needs_a_notarized_parent_and_timeouts_only_between() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        let mut parent = s.template(0, BlockHash::ZERO, 77.into());
        let parent_id = parent.candidate_id();
        s.round = 2;
        commit(&mut s, 1..=6, root);
        let child_id = propose(&mut s, parent_id, root).candidate_id();
        assert!(!s.complete(child_id), "the parent is unknown");
        // Notarized by certificate weight, short of a fast certificate.
        for i in 1..=4 {
            parent.sign(&PrivateKey::from(i));
            s.receive(parent.clone());
        }
        assert!(
            !s.complete(child_id),
            "the skipped round 1 has no timeout certificate"
        );
        assert_eq!(
            s.select_parent(),
            BlockHash::ZERO,
            "the selector skips only timed-out rounds as well"
        );
        for i in 1..=4 {
            let mut timeout = s.template(1, BlockHash::ZERO, BlockHash::ZERO);
            timeout.kind = 4;
            timeout.sign(&PrivateKey::from(i));
            s.receive(timeout);
        }
        assert!(
            s.complete(child_id),
            "the parent's own round needs no timeout and its membership need not be held"
        );
        assert_eq!(
            s.select_parent(),
            parent_id,
            "the selector picks the notarized parent an assembler extends"
        );
        s.enter_round(2);
        let (out, _) = s.drive(&[PrivateKey::from(1)]);
        assert_eq!(
            out.iter().map(|p| (p.kind, p.parent)).collect::<Vec<_>>(),
            vec![(8, BlockHash::ZERO), (0, parent_id)],
            "the announcement carries no parent; the complete child takes the FIRST vote"
        );
    }

    #[test]
    fn a_notarized_candidate_exits_the_round_without_a_timeout_certificate() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = PrivateKey::from(1);
        let (out, _) = s.drive(&[key.clone()]);
        let announced = out[0].clone();
        commit(&mut s, 1..=6, announced.state);
        propose(&mut s, BlockHash::ZERO, announced.state);
        let (out, _) = s.drive(&[key.clone()]);
        let candidate = out[0].clone();
        assert_eq!(candidate.kind, 0);
        for i in 2..=4 {
            let mut vote = candidate.clone();
            vote.sign(&PrivateKey::from(i));
            s.receive(vote);
        }
        let (out, decision) = s.drive(&[key.clone()]);
        assert!(decision.is_none(), "notarized, not finalized");
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![2],
            "FINAL follows completion, then the replica exits"
        );
        assert_eq!(s.round, 1);
        assert!(s.round_timer_armed);
        assert_eq!(
            s.select_parent(),
            candidate.candidate_id(),
            "round 1 extends the notarized candidate"
        );
        let (out, _) = s.drive(&[key]);
        assert!(
            out.is_empty(),
            "the unchanged root is not announced again; round 1 waits for its assembler"
        );
        assert!(
            !s.rounds[&0].certificate(BlockHash::ZERO, VoteKind::Timeout),
            "round 0 never timed out"
        );
    }

    #[test]
    fn the_decision_is_the_earliest_candidate_on_the_finalized_chain() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        let mut parent = s.template(0, BlockHash::ZERO, root);
        let parent_id = parent.candidate_id();
        // The child names a membership this replica never held, and is
        // explicitly (fast) finalized.
        let mut child = s.template(1, parent_id, 77.into());
        for i in 1..=5 {
            child.sign(&PrivateKey::from(i));
            s.receive(child.clone());
        }
        assert_eq!(s.decision(), None, "the parent is not certified here");
        assert!(!s.has_finalized_close());
        for i in 1..=4 {
            parent.sign(&PrivateKey::from(i));
            s.receive(parent.clone());
        }
        assert!(s.has_finalized_close());
        assert_eq!(s.decision(), Some(parent_id));
        let (_, decision) = s.drive(&[]);
        assert_eq!(
            decision,
            Some(vec![]),
            "the epoch closes on the ancestor's membership, not the descendant's"
        );
    }

    #[test]
    fn a_stalled_notarized_round_closes_through_its_descendant() {
        // One representative silent, two FIRST-timeout before the value was
        // due: X gets three FIRST and two second-look notarizations, so it is
        // notarized, but only three FINAL and too few timeouts to trigger
        // timeout shares (5 - 3 < 3). The round must exit on completion.
        let ledger = Ledger::new_null();
        let mut replicas: Vec<_> = (0..6).map(|_| ready_state(&ledger)).collect();
        let keys = sorted_keys();
        for replica in &mut replicas {
            replica.ready_since = Some(Instant::now() - AGREEMENT_TIMEOUT);
        }
        let mut packets = Vec::new();
        for i in 3..5 {
            let mut timeout = replicas[i].template(0, BlockHash::ZERO, BlockHash::ZERO);
            timeout.kind = 3;
            timeout.sign(&keys[i]);
            let signer = replicas[i]
                .rounds
                .entry(0)
                .or_default()
                .signers
                .entry(keys[i].public_key())
                .or_default();
            signer.first = Some(BlockHash::ZERO);
            signer.timeout = true;
            packets.push(timeout);
        }
        exchange(&mut replicas, packets);
        let mut decisions = vec![None; 5];
        for _ in 0..12 {
            let mut packets = Vec::new();
            for i in 0..5 {
                // The silent member never announces, so round 1's phase ends by
                // expiry: half the first-vote budget after entry.
                if replicas[i].round == 1 {
                    replicas[i].rounds.get_mut(&1).unwrap().started =
                        Instant::now() - round_timeout(1) / 2;
                }
                let (out, decision) = replicas[i].drive(&[keys[i].clone()]);
                if decision.is_some() {
                    decisions[i] = decision;
                }
                packets.extend(out);
            }
            exchange(&mut replicas, packets);
        }
        assert!(
            decisions.iter().all(|d| d.as_ref() == Some(&vec![])),
            "every active replica closes: {decisions:?}"
        );
        let round0 = &replicas[0].rounds[&0];
        assert!(!round0.certificate(BlockHash::ZERO, VoteKind::Timeout));
        let x = *replicas[0]
            .candidates
            .iter()
            .find(|(_, c)| c.header.round == 0 && !c.header.state.is_zero())
            .map(|(id, _)| id)
            .unwrap();
        assert!(round0.certificate(x, VoteKind::Notarize));
        assert!(!round0.certificate(x, VoteKind::Final));
        assert!(
            replicas[..5].iter().all(|r| r.decision() == Some(x)),
            "the round-0 value is finalized implicitly by its round-1 descendant"
        );
        assert!(replicas[..5].iter().all(|r| r.round == 1));
    }

    #[test]
    fn timeout_rotates_past_unavailable_target_despite_inflated_count() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let mut unavailable = s.template(0, BlockHash::ZERO, 99.into());
        unavailable.members = u64::MAX;
        unavailable.sign(&PrivateKey::from(1));
        s.receive(unavailable);
        for i in 1..=6 {
            let mut timeout = s.template(0, BlockHash::ZERO, BlockHash::ZERO);
            timeout.kind = 3;
            timeout.sign(&PrivateKey::from(i));
            s.receive(timeout);
        }
        s.drive(&[]);
        assert_eq!(s.round, 1);
    }

    #[test]
    fn previous_close_is_bound_and_wrong_history_is_rejected() {
        let mut s = state();
        s.advance(vec![], s.weights.clone(), &[]);
        let expected = s.previous_close;
        assert!(!expected.is_zero());
        let mut p = s.template(0, BlockHash::ZERO, Ledger::epoch_state_hash(1, &[]));
        p.previous_close = BlockHash::ZERO;
        p.sign(&PrivateKey::from(1));
        s.receive(p);
        assert!(s.candidates.is_empty());
    }

    #[test]
    fn epoch_close_recovery_limit_does_not_drop_authenticated_decision_headers() {
        let mut replica = state();
        replica.round = 64;
        for round in 0..64 {
            let mut vote = replica.template(round, BlockHash::ZERO, 1.into());
            vote.sign(&PrivateKey::from(1));
            replica.receive(vote);
        }
        let mut next = replica.template(65, BlockHash::ZERO, 1.into());
        next.sign(&PrivateKey::from(2));
        let id = next.candidate_id();
        replica.receive(next);
        assert!(
            replica.candidates.contains_key(&id),
            "the next authenticated decision header is retained"
        );
    }
    #[test]
    fn epoch_close_equivocation_does_not_allocate_candidate_headers() {
        let mut s = state();
        for value in 1..=100 {
            let mut p = s.template(0, BlockHash::ZERO, value.into());
            p.sign(&PrivateKey::from(1));
            s.receive(p);
        }
        assert_eq!(s.candidates.len(), 1);
    }

    #[test]
    fn epoch_close_archive_survives_two_advances_with_lagging_peer() {
        let mut s = state();
        let hashes = vec![1.into()];
        let digest = Ledger::epoch_state_hash(0, &hashes);
        let mut p = s.template(0, BlockHash::ZERO, digest);
        p.sign(&PrivateKey::from(1));
        s.receive(p.clone());
        s.advance(hashes.clone(), s.weights.clone(), &[PrivateKey::from(1)]);
        let closed = s.closed.as_ref().unwrap();
        assert_eq!(closed.trie.epoch(), 0);
        assert_eq!(closed.announcements.len(), 1);
        assert_eq!(closed.announcements[0].state, digest);
        assert_eq!(closed.announcements[0].epoch, 0);
        assert!(closed.announcements[0].valid_announcement());
        let mut receipt = s.template(0, BlockHash::ZERO, digest);
        receipt.epoch = 0;
        receipt.kind = 6;
        receipt.members = 0;
        receipt.sign(&PrivateKey::from(1));
        s.local_receipts.push(receipt.clone());
        s.advance(vec![1.into(), 2.into()], s.weights.clone(), &[]);
        assert!(s.archive.iter().any(|v| v == &p));
        assert_eq!(
            s.closed.as_ref().map(|c| c.trie.epoch()),
            Some(1),
            "only the latest close is served as a view"
        );
        for i in 1..=6 {
            receipt.sign(&PrivateKey::from(i));
            s.receive(receipt.clone());
        }
        assert!(s.archive_acknowledged(0, digest));
    }

    #[test]
    fn epoch_close_archive_requires_matching_receipts_from_every_pr() {
        let mut s = state();
        s.epoch = 1;
        let digest = BlockHash::from(10);
        for i in 1..=6 {
            assert!(!s.archive_acknowledged(0, digest));
            let mut receipt = s.template(0, BlockHash::ZERO, digest);
            receipt.epoch = 0;
            receipt.kind = 6;
            receipt.sign(&PrivateKey::from(i));
            let mut forged = receipt.clone();
            forged.state = 11.into();
            s.receive(forged);
            assert!(!s.archive_acknowledged(0, digest));
            s.receive(receipt);
        }
        assert!(s.archive_acknowledged(0, digest));
        assert!(!s.archive_acknowledged(0, 11.into()));
        assert!(s.rounds.is_empty());
    }

    #[test]
    fn epoch_close_rounds_allow_more_time_for_delayed_snapshots() {
        assert_eq!(round_timeout(0), Duration::from_secs(3));
        assert_eq!(round_timeout(3), Duration::from_secs(24));
        assert_eq!(round_timeout(10), Duration::from_secs(3072));
        assert!(round_timeout(40) > round_timeout(39), "no cap");
        assert_eq!(round_timeout(u64::MAX), Duration::from_secs(u64::MAX));
    }

    #[test]
    fn epoch_deadlines_share_an_anchor_without_drifting_after_close() {
        let start = Instant::now();
        assert!(!epoch_due(start, start + Duration::from_secs(24), 0, 25));
        assert!(epoch_due(start, start + Duration::from_secs(25), 0, 25));
        assert!(!epoch_due(start, start + Duration::from_secs(49), 1, 25));
        assert!(epoch_due(start, start + Duration::from_secs(50), 1, 25));
        assert!(epoch_due(start, start + Duration::from_secs(80), 1, 25));
        assert!(!epoch_due(start + Duration::from_secs(1), start, 0, 25));
    }

    #[test]
    fn epoch_close_retransmission_is_bounded_and_prefers_newest_rounds() {
        let mut s = state();
        s.round = 19;
        for round in 0..20 {
            for rep in 1..=6 {
                let mut p = s.template(round, BlockHash::ZERO, (round * 10 + rep).into());
                p.sign(&PrivateKey::from(rep));
                s.receive(p);
                let mut timeout = s.template(round, BlockHash::ZERO, BlockHash::ZERO);
                timeout.kind = 4;
                timeout.sign(&PrivateKey::from(rep));
                s.receive(timeout);
            }
        }
        let expected = s.packets().len();
        assert_eq!(expected, 240);
        s.queue_retransmission();
        let first = s.retransmission_burst();
        assert_eq!(first.len(), RETRANSMIT_BURST);
        assert_eq!(first.iter().filter(|p| p.round == 19).count(), 12);
        assert!(first.iter().all(|p| p.round >= 18));
        // A cycle in progress is completed rather than restarted or duplicated.
        s.queue_retransmission();
        let mut sent: Vec<_> = first;
        loop {
            let burst = s.retransmission_burst();
            if burst.is_empty() {
                break;
            }
            assert!(burst.len() <= RETRANSMIT_BURST);
            sent.extend(burst);
        }
        assert_eq!(sent.len(), expected);
        let rounds: Vec<_> = sent.iter().map(|p| p.round).collect();
        assert!(rounds.windows(2).all(|w| w[0] >= w[1]));
        let unique: HashSet<_> = sent
            .iter()
            .map(|p| (p.voter, p.kind, p.round, p.state))
            .collect();
        assert_eq!(unique.len(), expected);
    }
    #[test]
    fn epoch_close_votes_are_deduplicated_and_separated_by_round() {
        let mut s = state();
        let mut p = s.template(0, BlockHash::ZERO, 1.into());
        let id = p.candidate_id();
        for rep in 1..=4 {
            p.sign(&PrivateKey::from(rep));
            s.receive(p.clone());
            s.receive(p.clone());
        }
        assert!(s.rounds[&0].certificate(id, VoteKind::Notarize));
        assert!(!s.rounds[&0].certificate(id, VoteKind::First));
        let mut other = p.clone();
        other.round = 1;
        other.sign(&PrivateKey::from(5));
        s.receive(other);
        assert!(!s.rounds[&0].certificate(id, VoteKind::First));
        p.sign(&PrivateKey::from(5));
        s.receive(p);
        assert!(s.rounds[&0].certificate(id, VoteKind::First));
    }

    #[test]
    fn epoch_close_digest_only_votes_bind_to_local_membership() {
        use rsnano_ledger::{LedgerBuilder, LedgerConstants};
        let path = std::env::temp_dir().join(format!("rai-close-digest-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(40).unwrap();
            let snapshot = ledger.epoch_close_candidate(0);
            let digest = Ledger::epoch_state_hash(0, &snapshot);
            let mut replica = state();
            replica.sync_membership(&ledger);
            for i in 1..=6 {
                let mut vote = replica.template(0, BlockHash::ZERO, digest);
                vote.sign(&PrivateKey::from(i));
                replica.receive(vote);
            }
            let (out, decision) = replica.drive(&[]);
            assert!(out.is_empty());
            assert_eq!(
                decision.as_ref(),
                Some(&snapshot),
                "a matching root needs no pages"
            );
        }
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn epoch_close_rejects_forged_votes_and_unjustified_future_rounds() {
        let mut s = state();
        let mut p = s.template(0, BlockHash::ZERO, 1.into());
        p.sign(&PrivateKey::from(1));
        p.state = 2.into();
        s.receive(p);
        assert!(s.rounds.is_empty());
        let mut p = s.template(FUTURE_ROUNDS + 1, BlockHash::ZERO, 1.into());
        p.sign(&PrivateKey::from(1));
        s.receive(p);
        assert!(s.rounds.is_empty(), "rounds beyond the window are rejected");
        let mut p = s.template(9, BlockHash::ZERO, 1.into());
        p.sign(&PrivateKey::from(1));
        s.receive(p);
        assert!(
            s.rounds.contains_key(&9),
            "a bounded future round is stored so a lagging replica can catch up"
        );
    }

    #[test]
    fn solicitation_batches_rotate_through_every_target() {
        let mut s = state();
        let targets: Vec<(BlockHash, Root)> = (1..=600u64)
            .map(|i| (BlockHash::from(i), Root::from(0u64)))
            .collect();
        let now = Instant::now();
        let first = s.next_solicitation_batch(&targets, now);
        let second = s.next_solicitation_batch(&targets, now);
        let third = s.next_solicitation_batch(&targets, now);
        assert_eq!(
            (first.len(), second.len(), third.len()),
            (
                rsnano_messages::ConfirmReq::HASHES_MAX,
                rsnano_messages::ConfirmReq::HASHES_MAX,
                90
            )
        );
        let seen: HashSet<_> = first
            .iter()
            .chain(&second)
            .chain(&third)
            .map(|(h, _)| *h)
            .collect();
        assert_eq!(
            seen.len(),
            600,
            "every target solicited exactly once per cycle"
        );
        assert!(
            s.next_solicitation_batch(&targets, now).is_empty(),
            "all throttled"
        );
        for last in s.solicited.values_mut() {
            *last -= SOLICIT_INTERVAL;
        }
        let again = s.next_solicitation_batch(&targets, now);
        assert_eq!(again.len(), rsnano_messages::ConfirmReq::HASHES_MAX);
        assert_eq!(
            again[0].0,
            BlockHash::from(511),
            "the cycle resumes where it stopped"
        );
    }

    #[test]
    fn reconciliation_targets_are_solicited_at_their_own_cadence() {
        let mut s = state();
        let members = vec![BlockHash::from(9), BlockHash::from(10)];
        for member in &members {
            s.reconcile_targets
                .insert(*member, (Root::from(0u64), None));
        }
        let now = Instant::now();
        let all = |hashes: &[BlockHash]| hashes.iter().copied().collect::<HashSet<_>>();
        let mut targets = s.reconcile_batch(now, all);
        targets.sort();
        assert_eq!(
            targets.iter().map(|(hash, _)| *hash).collect::<Vec<_>>(),
            members
        );
        assert!(
            s.reconcile_batch(now, all).is_empty(),
            "solicitations are rate limited per member"
        );
        assert_eq!(s.reconcile_batch(now + RECONCILE_INTERVAL, all).len(), 2);
        let known = |_: &[BlockHash]| HashSet::from([BlockHash::from(9)]);
        assert_eq!(
            s.reconcile_batch(now + RECONCILE_INTERVAL * 2, known).len(),
            1,
            "a member learned since is no longer solicited"
        );
    }

    #[test]
    fn level1_bucket_counts_agree_between_messages_and_the_trie() {
        assert_eq!(EpochClose::LEVEL1_BUCKETS, rsnano_ledger::LEVEL1_BUCKETS);
        assert_eq!(EpochClose::SKETCH_BYTES, rsnano_ledger::SKETCH_BYTES);
        let hash = BlockHash::from(0x1234u64 << 48);
        assert_eq!(
            EpochClose::prefix_of(&hash),
            MembershipTrie::prefix_of(&hash)
        );
    }

    /* Test helpers */

    fn state() -> State {
        let weights: Vec<_> = (1..=6)
            .map(|i| (PrivateKey::from(i).public_key(), Amount::raw(100)))
            .collect();
        let mut w = RepWeights::default();
        for (rep, amount) in weights {
            w.put(rep, amount);
        }
        State::new(0, w, vec![])
    }

    fn ready_state(ledger: &Ledger) -> State {
        let mut s = state();
        s.draining = true;
        s.ready = true;
        s.sync_membership(ledger);
        s
    }

    fn unknown_members(ledger: &Ledger, hashes: &[BlockHash]) -> Vec<(BlockHash, Root)> {
        ledger
            .epoch_close_missing_members(0, hashes)
            .into_iter()
            .map(|hash| (hash, Root::from(0u64)))
            .collect()
    }

    /// Round announcements of representatives `reps` for `state`.
    fn commit(s: &mut State, reps: std::ops::RangeInclusive<u64>, state: BlockHash) {
        let mut commitment = s.template(s.round, BlockHash::ZERO, state);
        commitment.kind = 10;
        commitment.members = 0;
        for i in reps {
            commitment.sign(&PrivateKey::from(i));
            s.receive_commitment(commitment.clone());
        }
    }

    /// The key of the current round's assembler.
    fn assembler_key(s: &State) -> PrivateKey {
        (1..=6)
            .map(PrivateKey::from)
            .find(|key| Some(key.public_key()) == s.assembler(s.round))
            .unwrap()
    }

    /// The assembler's proposal of `(parent, state)` for the current round,
    /// carrying the announcements `s` holds for the target, delivered to `s`.
    fn propose(s: &mut State, parent: BlockHash, state: BlockHash) -> EpochClose {
        let key = assembler_key(s);
        let certificate = s
            .rounds
            .get(&s.round)
            .map(|round| {
                round
                    .commitments
                    .values()
                    .filter(|c| c.state == state)
                    .map(|c| CloseSigner {
                        voter: c.voter,
                        signature: c.signature.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut proposal = s.template(s.round, parent, state);
        proposal.kind = 11;
        proposal.certificate = certificate;
        proposal.sign(&key);
        s.receive_proposal(proposal.clone());
        proposal
    }

    fn sorted_keys() -> Vec<PrivateKey> {
        let mut keys: Vec<_> = (1..=6).map(PrivateKey::from).collect();
        keys.sort_by_key(|k| k.public_key());
        keys
    }

    fn exchange(replicas: &mut [State], packets: Vec<EpochClose>) {
        for replica in replicas.iter_mut() {
            for packet in &packets {
                if packet.kind == 8 {
                    replica.receive_announcement(packet.clone(), ChannelId::LOOPBACK);
                } else if packet.kind == 10 {
                    replica.receive_commitment(packet.clone());
                } else if packet.kind == 11 {
                    replica.receive_proposal(packet.clone());
                } else {
                    replica.receive(packet.clone());
                }
            }
        }
    }
}
