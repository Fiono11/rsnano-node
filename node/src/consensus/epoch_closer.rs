//! Kudzu close rounds with a rotating close assembler (RAI.tex A0, C0-C4).
//! Memberships converge during the epoch through the elections themselves:
//! an election that stays unsettled longer than elections take to settle
//! (the container's running estimate) has lost a vote somewhere, and its
//! representatives are asked for their votes once, in batches; once the
//! close is due every unsettled election is asked at once. A replica is
//! close-ready only when every election of the epoch is decided and settled,
//! that is, every certificate of it has been collected here or proved
//! impossible, so every drained replica holds the same membership. The close
//! then needs no announcement and no reconciliation: the round's assembler
//! proposes its own membership root
//! paired with a ParentOK parent (C2) and a replica FIRST-votes or notarizes
//! the proposal only after checking the assembler's signature, its own
//! previous close, ParentOK and that the proposed root is exactly its own
//! drained membership. Its vote carries its own D3/D4 judgment, so a member
//! learned after the vote never withdraws it. Voting epochs and canonical
//! ledger epochs are independent.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rsnano_ledger::{Ledger, MembershipTrie, RepWeights};
use rsnano_messages::{EpochClose, Message};
use rsnano_network::{ChannelId, TrafficType};
use rsnano_types::{
    Amount, BlockHash, ElectionId, PrivateKey, PublicKey, Root, Signature, Vote, VoteKind,
};
use rsnano_utils::{CancellationToken, ticker::Tickable};

use crate::{
    consensus::{AecService, VoteGenerators, election::kudzu::KudzuVotes},
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
/// Spacing of the re-flood of the current round's proposal and this replica's
/// own votes in it. Under load a peer's inbound queue drops packets, and the
/// first-vote budget of a round is a few seconds, so the packets that decide
/// the current round are repeated well within it; the rest of the close
/// history follows the `RETRANSMIT` cycle.
const REFRESH: Duration = Duration::from_millis(500);
/// Packets flooded per tick. A channel write queue holds 64 messages per traffic
/// type and `try_send` drops on overflow, so a synchronous flood of a whole close
/// archive loses the same tail on every retransmission. Bounded bursts at the
/// 100 ms tick cadence stay well below that capacity.
const RETRANSMIT_BURST: usize = 16;
/// Rounds ahead of the local round whose votes are stored on arrival. Rounds end
/// as fast as votes propagate while digests differ, so a replica that misses one
/// packet would otherwise crawl one round per retransmission cycle behind peers.
const FUTURE_ROUNDS: u64 = 256;
/// Minimum spacing of direct solicitations for one election. Peers answer at once
/// and their reply cache suppresses repeats for five seconds, so a shorter spacing
/// would only add request and reply load to the workload.
const SOLICIT_INTERVAL: Duration = Duration::from_secs(10);
/// Time allowed for every representative's reply to a solicitation of an
/// unsettled election, before that election counts as settled here.
const REPLY_WAIT: Duration = Duration::from_secs(2);
/// The same once the close is due: every unsettled election is solicited at
/// once, whatever its age, and replies are awaited only as long as they take
/// under load, so the close converges as fast as the network allows.
const DRAIN_REPLY_WAIT: Duration = Duration::from_secs(1);
/// Spacing of the settlement scan outside a drain. Each scan solicits, in one
/// batch, the elections that have stayed unsettled longer than elections
/// take to settle, so memberships converge during the epoch, the drain
/// finds them settled, and only lost votes are asked for.
const SETTLE_SCAN: Duration = Duration::from_secs(1);
/// Proposals stored per round. A correct assembler signs one value per round;
/// an equivocating one may pair its root with several parents, and a bounded
/// number of them can still be notarized on second look.
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
    started: Instant,
}
impl Default for Round {
    fn default() -> Self {
        Self {
            tally: Default::default(),
            votes: Default::default(),
            signers: Default::default(),
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
    /// The round assembler's signed proposal of this value (C2(a)); retained
    /// and rebroadcast so a second-look replica can run the same checks.
    proposal: Option<EpochClose>,
}

struct State {
    draining: bool,
    /// This replica's FIRST obligations in `epoch`, with the block each voted
    /// for, frozen when the drain began.
    local_first: Arc<Vec<(ElectionId, BlockHash)>>,
    epoch: u64,
    round: u64,
    previous_close: BlockHash,
    ready: bool,
    rounds: BTreeMap<u64, Round>,
    candidates: BTreeMap<BlockHash, Candidate>,
    weights: RepWeights,
    total: Amount,
    last_send: Instant,
    last_refresh: Instant,
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
    /// When this replica began draining `epoch`. The drain's timeout votes
    /// and second looks create certificates, so a solicitation answered
    /// before it may have missed them: such elections are asked once more.
    drain_since: Option<Instant>,
    /// Round 0 is timed from this replica's own readiness, not from the drain
    /// start; later rounds from their entry.
    round_timer_armed: bool,
    /// Live membership of `epoch`, followed incrementally from the ledger.
    trie: MembershipTrie,
    /// The same members in recording order (the ledger's candidate log), so
    /// the membership a validated root named is reproducible after the live
    /// one grew.
    log: Vec<BlockHash>,
    /// Roots this replica validated as exactly its own drained membership,
    /// with the log length at that time (C2(e)). Validation is permanent:
    /// a member learned later never withdraws it (K9 monotonicity).
    validated: HashMap<BlockHash, usize>,
    /// The last (decided root, live root) pair reported as lagging, so the
    /// report is written once per change.
    lag_reported: Option<(BlockHash, BlockHash)>,
    /// Decided elections whose certificates may still be incomplete here, by
    /// winner hash, with the time this replica solicited every representative
    /// for them after their decision.
    unsettled: HashMap<BlockHash, Instant>,
    last_settle_report: Option<Instant>,
    last_settle_scan: Option<Instant>,
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
            last_refresh: Instant::now() - REFRESH,
            archive,
            receipts: BTreeMap::new(),
            local_receipts: Vec::new(),
            retransmit: Default::default(),
            solicited: Default::default(),
            last_timeout_solicitation: None,
            recovery_cursor: 0,
            ready_traced: false,
            ready_since: None,
            drain_since: None,
            round_timer_armed: false,
            trie: MembershipTrie::new(epoch),
            log: Vec::new(),
            validated: Default::default(),
            lag_reported: None,
            unsettled: Default::default(),
            last_settle_report: None,
            last_settle_scan: None,
        }
    }
    /// Close readiness beyond the drain (D3): every decided election has
    /// every certificate collected. An election settles when no further
    /// certificate can exist, or once every representative has been asked
    /// for its votes and had time to answer. It is asked only after it has
    /// been open longer than elections take to settle (`threshold`, the
    /// container's running estimate): staying unsettled past that is the
    /// sign of a lost vote, and asking earlier would request what is still
    /// in flight. Returns whether all are settled and the batch to solicit.
    fn settle(
        &mut self,
        unsettled: &[(BlockHash, Root, Duration)],
        threshold: Duration,
        reply_wait: Duration,
        now: Instant,
    ) -> (bool, Vec<(BlockHash, Root)>) {
        let mut requests = Vec::new();
        let mut settled = true;
        let mut live = HashSet::new();
        for (hash, root, since_start) in unsettled {
            live.insert(*hash);
            match self.unsettled.get(hash) {
                Some(asked) if self.drain_since.is_some_and(|drain| *asked < drain) => {
                    // Answered before the drain began: ask once more.
                    self.unsettled.insert(*hash, now);
                    requests.push((*hash, *root));
                    settled = false;
                }
                Some(asked) => {
                    if now.duration_since(*asked) < reply_wait {
                        settled = false;
                    }
                }
                None if *since_start < threshold => settled = false,
                None => {
                    self.unsettled.insert(*hash, now);
                    requests.push((*hash, *root));
                    settled = false;
                }
            }
        }
        self.unsettled.retain(|hash, _| live.contains(hash));
        (settled, requests)
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
        // A later round follows exit evidence every replica observed at about
        // the same time. Round 0 waits for this replica's own readiness.
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
        let (added, _) = ledger.epoch_candidates_since(self.epoch, self.log.len());
        for member in &added {
            if self.trie.insert(*member) {
                self.log.push(*member);
            }
        }
        added
    }
    fn root(&mut self) -> BlockHash {
        self.trie.root()
    }
    /// The members a validated or live root names: the live membership for
    /// the live root, otherwise the prefix of the log recorded when the root
    /// was validated.
    fn members_of(&mut self, state: BlockHash) -> Option<Vec<BlockHash>> {
        if state == self.root() {
            return Some(self.trie.members().copied().collect());
        }
        let length = *self.validated.get(&state)?;
        let mut members = self.log[..length].to_vec();
        members.sort_unstable();
        Some(members)
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
    /// Assembly (rule C2): when one of `keys` is the current round's
    /// assembler and this replica is close-ready, pair its own membership
    /// root with the parent the K9 selector picks from the validated close
    /// cache, sign the one proposal of the round and publish it.
    fn propose(&mut self, keys: &[PrivateKey], out: &mut Vec<EpochClose>) {
        if !self.ready {
            return;
        }
        let Some(assembler) = self.assembler(self.round) else {
            return;
        };
        let Some(key) = keys.iter().find(|key| key.public_key() == assembler) else {
            return;
        };
        let round = self.rounds.entry(self.round).or_default();
        let signer = round.signers.entry(assembler).or_default();
        if signer.proposed {
            return;
        }
        signer.proposed = true;
        let state = self.root();
        let parent = self.select_parent();
        let mut proposal = self.template(self.round, parent, state);
        proposal.kind = EpochClose::PROPOSAL;
        proposal.sign(key);
        debug_trace(
            || serde_json::json!({"type":"proposal","epoch":self.epoch,"round":self.round,"id":proposal.candidate_id(),"parent":parent,"state":state,"members":proposal.members,"rep":assembler}),
        );
        self.receive_proposal(proposal.clone());
        out.push(proposal);
    }
    /// Store the round assembler's proposal (C2(a), with C2(c) for the
    /// previous close). A proposal by anyone but the round's assembler is
    /// ignored, as is any beyond the equivocation bound of a round; the first
    /// proposal of a value is kept.
    fn receive_proposal(&mut self, p: EpochClose) {
        if p.epoch != self.epoch
            || p.previous_close != self.previous_close
            || p.round > self.round + FUTURE_ROUNDS
            || self.assembler(p.round) != Some(p.voter)
            || !p.valid_proposal()
        {
            return;
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
        let candidate = self.candidates.entry(id).or_insert_with(|| Candidate {
            header: p.clone(),
            proposal: None,
        });
        if candidate.proposal.is_none() {
            debug_trace(
                || serde_json::json!({"type":"proposal_received","epoch":p.epoch,"round":p.round,"id":id,"state":p.state,"rep":p.voter}),
            );
            candidate.proposal = Some(p);
        }
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
    /// The packets that decide the current round: its proposals and the votes
    /// of `keys` in it, re-flooded every `REFRESH` while the round is open.
    fn refresh_packets(&mut self, keys: &[PrivateKey], now: Instant) -> Vec<EpochClose> {
        if !self.draining || now.duration_since(self.last_refresh) < REFRESH {
            return Vec::new();
        }
        self.last_refresh = now;
        let mut packets: Vec<_> = self
            .candidates
            .values()
            .filter(|c| c.header.round == self.round)
            .filter_map(|c| c.proposal.clone())
            .collect();
        if let Some(round) = self.rounds.get(&self.round) {
            packets.extend(
                round
                    .votes
                    .iter()
                    .filter(|((voter, _, _), _)| keys.iter().any(|key| key.public_key() == *voter))
                    .map(|(_, vote)| vote.clone()),
            );
        }
        packets
    }
    /// Move to the next epoch after closing this one with `hashes`. The vote
    /// archive is retransmitted until every representative acknowledged the
    /// close, so a lagging replica still learns the decision.
    fn advance(&mut self, hashes: &[BlockHash], weights: RepWeights) {
        let mut archive = std::mem::take(&mut self.archive);
        archive.extend(self.packets());
        let receipts = std::mem::take(&mut self.receipts);
        let local_receipts = std::mem::take(&mut self.local_receipts);
        let epoch = self.epoch;
        let digest = Ledger::epoch_state_hash(epoch, hashes);
        let previous_close = rsnano_types::Blake2HashBuilder::new()
            .update(b"RAI-CLOSE")
            .update(epoch.to_le_bytes())
            .update(self.previous_close.as_bytes())
            .update(digest.as_bytes())
            .build();
        *self = State::new(epoch + 1, weights, archive);
        self.previous_close = previous_close;
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
            members: self.trie.len() as u64,
        }
    }
    /// Votes and receipts. Proposals have their own entry point.
    fn receive(&mut self, p: EpochClose) {
        if p.kind == EpochClose::RECEIPT {
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
        self.candidates.keys().any(|id| self.finalized(*id))
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
    /// C2(e) and D3/D4 in one check: the target is exactly this replica's own
    /// drained membership, now or when it was first validated. A drained
    /// replica attests that every election it knows has terminated and that
    /// the target omits none of its members; a member learned later never
    /// withdraws a validation already made.
    fn holds(&mut self, state: BlockHash) -> bool {
        if self.validated.contains_key(&state) {
            return true;
        }
        if self.ready && state == self.root() {
            self.validated.insert(state, self.log.len());
            return true;
        }
        false
    }
    /// C2(a)-(e) for a candidate: the round assembler proposed it, ParentOK
    /// holds and its target is this replica's own drained membership; (c)
    /// held when the proposal was stored. Such a candidate takes a FIRST vote
    /// or a second-look share, and is complete once notarized.
    fn complete(&mut self, id: BlockHash) -> bool {
        let Some(c) = self.candidates.get(&id) else {
            return false;
        };
        let state = c.header.state;
        c.proposal.is_some() && self.well_formed(id) && self.holds(state)
    }
    fn sign(&mut self, mut p: EpochClose, key: &PrivateKey, k: u8, out: &mut Vec<EpochClose>) {
        p.kind = k;
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
    fn packets(&self) -> Vec<EpochClose> {
        self.rounds
            .values()
            .flat_map(|r| r.votes.values().cloned())
            .collect()
    }

    fn drive(&mut self, keys: &[PrivateKey]) -> (Vec<EpochClose>, Option<Vec<BlockHash>>) {
        let mut out = Vec::new();
        if self.ready && self.ready_since.is_none() {
            self.ready_since = Some(Instant::now());
        }
        // The decided candidate closes the epoch once its membership is held
        // here: the live one, or the one validated before the live one grew.
        if let Some(id) = self.decision() {
            let state = self.candidates[&id].header.state;
            if let Some(members) = self.members_of(state) {
                return (out, Some(members));
            }
            // Lagging: the membership keeps converging through the elections
            // until it reaches the decided root. Reported once per change.
            let root = self.root();
            if self.lag_reported != Some((state, root)) {
                self.lag_reported = Some((state, root));
                eprintln!(
                    "EPOCH_CLOSE_LAGGING {}",
                    serde_json::json!({"pid":std::process::id(),"epoch":self.epoch,"decided":state,"state":root,"members":self.trie.len()})
                );
                debug_trace(
                    || serde_json::json!({"type":"lagging","epoch":self.epoch,"decided":state,"state":root,"members":self.trie.len()}),
                );
            }
            return (out, None);
        }
        // Readiness gates proposing and voting; every draining replica tallies.
        if !self.draining || self.has_finalized_close() {
            return (out, None);
        }
        self.rounds.entry(self.round).or_default();
        // Round 0 is timed from this replica's own readiness: the first-vote
        // budget starts when it can validate a proposal (C2).
        if self.ready && !self.round_timer_armed {
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
        // C2(a)-(e) complete before the deadline. Past the deadline the vote is
        // a timeout below. With a correct assembler there is one proposal, so
        // a Byzantine member cannot split the correct first votes.
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
                            || serde_json::json!({"type":"first_vote","epoch":self.epoch,"round":self.round,"id":id,"parent":p.parent,"state":p.state,"rep":key.public_key()}),
                        );
                        self.sign(p.clone(), key, 0, &mut out);
                    }
                }
            }
        }
        for (id, p) in &current {
            // A second-look share re-runs C2(a)-(e) (C3). A FINAL vote is
            // bound to the FIRST vote only (FV): the validation this replica
            // made is not withdrawn by a member it learned since.
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
            // Certified ancestors are retained across rounds.
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
    incoming: Mutex<VecDeque<EpochClose>>,
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
    pub fn receive(&self, message: EpochClose, _channel: ChannelId) {
        let mut q = self.incoming.lock().unwrap();
        if q.len() < 4096 {
            q.push_back(message);
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
        let mut phase_ms: BTreeMap<&str, u128> = BTreeMap::new();
        let mut phase = Instant::now();
        let lap = |name: &'static str, phase: &mut Instant, phase_ms: &mut BTreeMap<&str, u128>| {
            phase_ms.insert(name, phase.elapsed().as_millis());
            *phase = Instant::now();
        };
        lap("keys", &mut phase, &mut phase_ms);
        for p in incoming {
            if p.kind == EpochClose::PROPOSAL {
                state.receive_proposal(p);
            } else {
                state.receive(p);
            }
        }
        lap("incoming", &mut phase, &mut phase_ms);
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
                state.drain_since = Some(Instant::now());
                state.enter_round(0);
                debug_trace(
                    || serde_json::json!({"type":"drain_start","epoch":state.epoch,"local_first":state.local_first.len()}),
                );
            }
        }
        lap("deadline", &mut phase, &mut phase_ms);
        // D3 is live, not a sticky flag: newly visible elections also block signing.
        if state.has_finalized_close() {
            self.generators.seal_epoch(state.epoch);
        }
        lap("seal", &mut phase, &mut phase_ms);
        // Readiness only matters while draining, so the container is scanned
        // only then; before that the tick must not stall vote application.
        let mut pending_targets = Vec::new();
        let readiness_started = Instant::now();
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
                .with_close_readiness(state.epoch, &local_first, |drained, pending, unsettled| {
                    pending_targets = pending;
                    let now = Instant::now();
                    // The close is due: no election is given more time to
                    // settle by itself.
                    let (unsettled, _) = unsettled;
                    let (settled, requests) =
                        state.settle(&unsettled, Duration::ZERO, DRAIN_REPLY_WAIT, now);
                    drain_requests.extend(requests);
                    if drained
                        && !settled
                        && state
                            .last_settle_report
                            .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(5))
                    {
                        state.last_settle_report = Some(now);
                        eprintln!(
                            "EPOCH_SETTLE_WAIT {}",
                            serde_json::json!({"pid":std::process::id(),"epoch":state.epoch,"unsettled":unsettled.len(),
                                "examples":unsettled.iter().take(4).map(|(hash, _, since)| serde_json::json!({"hash":hash,"open_ms":since.as_millis()})).collect::<Vec<_>>()})
                        );
                    }
                    state.ready = drained && settled;
                    state.drive(&keys)
                })
        } else {
            state.ready = false;
            // Settlement runs throughout the epoch, at a cadence that keeps
            // the container lock free for vote application.
            let now = Instant::now();
            if state
                .last_settle_scan
                .is_none_or(|last| now.duration_since(last) >= SETTLE_SCAN)
            {
                state.last_settle_scan = Some(now);
                let (unsettled, threshold) = self.aec.unsettled_epoch_elections(state.epoch);
                let (_, requests) = state.settle(&unsettled, threshold, REPLY_WAIT, now);
                if !requests.is_empty() {
                    debug_trace(
                        || serde_json::json!({"type":"settle_requests","epoch":state.epoch,"count":requests.len(),"unsettled":unsettled.len(),"threshold_ms":threshold.as_millis()}),
                    );
                }
                drain_requests.extend(requests);
            }
            state.drive(&keys)
        };
        let readiness_ms = readiness_started.elapsed().as_millis();
        lap("readiness", &mut phase, &mut phase_ms);
        let cycle = state.last_send.elapsed() >= RETRANSMIT;
        if state.ready && !state.ready_traced {
            state.ready_traced = true;
            let root = state.root();
            debug_trace(
                || serde_json::json!({"type":"ready","epoch":state.epoch,"round":state.round,"members":state.trie.len(),"state":root}),
            );
            // The membership itself is written off the tick: serializing it
            // inline costs seconds and delays every packet of the round.
            if let Some(dir) = std::env::var_os("RAI_CLOSE_TRACE_DIR") {
                let path = std::path::Path::new(&dir).join(format!(
                    "{}-members-{}-{}.txt",
                    std::process::id(),
                    state.epoch,
                    root
                ));
                let members: Vec<String> = state.trie.members().map(|m| m.to_string()).collect();
                std::thread::spawn(move || {
                    let _ = std::fs::write(path, members.join("\n"));
                });
            }
        }
        lap("ready_trace", &mut phase, &mut phase_ms);
        // Solicit the elections the drain is waiting for, including the local
        // FIRST obligations whose election is gone, at the retransmission cadence.
        let mut recovering = 0;
        if state.draining
            && closed.is_none()
            && state
                .last_timeout_solicitation
                .is_none_or(|last| last.elapsed() >= RETRANSMIT)
        {
            state.last_timeout_solicitation = Some(Instant::now());
            let now = Instant::now();
            // The container's undecided index plus the FIRST obligations
            // frozen at drain start name every election the drain waits for;
            // scanning the generators' statements for them instead walks the
            // whole epoch under their lock and stalls the tick for seconds.
            let targets = state.next_solicitation_batch(&pending_targets, now);
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
        lap("solicit", &mut phase, &mut phase_ms);
        if state.ready && cycle {
            let root = state.root();
            eprintln!(
                "EPOCH_CLOSE_PROGRESS {}",
                serde_json::json!({
                    "rep": keys.first().map(|k| k.public_key()), "epoch":state.epoch, "round":state.round,
                    "members":state.trie.len(),
                    "state":root,
                    "assembler":state.assembler(state.round),
                    "ready_for_ms":state.ready_since.map(|since| since.elapsed().as_millis()),
                    "proposals":state.candidates.values().filter_map(|c| c.proposal.as_ref()).map(|p| serde_json::json!({"round":p.round,"parent":p.parent,"state":p.state,"members":p.members,"rep":p.voter})).collect::<Vec<_>>(),
                    "round_timer_armed":state.round_timer_armed,
                    "recovering":recovering,
                    "unsettled":state.unsettled.len(),
                    "candidates":state.candidates.iter().map(|(id,c)| serde_json::json!({"id":id,"round":c.header.round,"state":c.header.state,"members":c.header.members})).collect::<Vec<_>>(),
                    "votes":state.rounds.iter().map(|(r,v)| (*r,v.votes.len())).collect::<BTreeMap<_,_>>()
                })
            );
        }

        lap("progress", &mut phase, &mut phase_ms);
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
                state.advance(&hashes, self.ledger.rep_weights.read().clone());
                for key in &keys {
                    let mut receipt = state.template(0, BlockHash::ZERO, digest);
                    receipt.epoch = epoch;
                    receipt.kind = EpochClose::RECEIPT;
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
        lap("close", &mut phase, &mut phase_ms);
        if let Some(receipt) = state.local_receipts.first() {
            if state
                .local_receipts
                .iter()
                .all(|r| state.archive_acknowledged(r.epoch, r.state))
                && !state.archive.is_empty()
            {
                debug_trace(
                    || serde_json::json!({"type":"archive_acknowledged","epoch":receipt.epoch,"state":receipt.state,"packets":state.archive.len()}),
                );
                state.archive.clear();
            }
        }
        if cycle {
            state.queue_retransmission();
            state.last_send = Instant::now();
        }
        outgoing.extend(state.retransmission_burst());
        outgoing.extend(state.refresh_packets(&keys, Instant::now()));
        let drain_epoch = state.epoch;
        drop(state);
        let outgoing_count = outgoing.len();
        let processing_ms = tick_started.elapsed().as_millis();
        let flood_started = Instant::now();
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
        if report_transport || tick_started.elapsed() >= Duration::from_millis(200) {
            eprintln!(
                "EPOCH_CLOSE_TRANSPORT {}",
                serde_json::json!({
                    "pid":std::process::id(),"unix_us":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros(),
                    "incoming":incoming_count,"outgoing":outgoing_count,
                    "processing_ms":processing_ms,"readiness_ms":readiness_ms,"phases":phase_ms,
                    "flood_ms":flood_started.elapsed().as_millis(),"total_ms":tick_started.elapsed().as_millis(),
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
        for _ in 0..6 {
            let mut packets = Vec::new();
            for i in 0..5 {
                let (out, decision) = replicas[i].drive(&[keys[i].clone()]);
                if decision.is_some() {
                    decisions[i] = decision;
                }
                packets.extend(out);
            }
            exchange(&mut replicas, packets);
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
    }

    #[test]
    fn a_replica_lacking_a_member_votes_once_it_learns_it() {
        let ledgers: Vec<_> = (0..6).map(|_| Ledger::new_null()).collect();
        let extra: rsnano_types::Block = rsnano_types::SavedBlock::new_test_instance().into();
        for ledger in &ledgers[..5] {
            ledger.record_epoch_block(0, extra.clone());
        }
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
        // Proposal, FIRST votes, FINAL votes: the five agreeing replicas close.
        let mut decisions = vec![None; 6];
        for _ in 0..4 {
            for (i, decision) in round(&mut replicas).into_iter().enumerate() {
                if decision.is_some() {
                    decisions[i] = decision;
                }
            }
        }
        assert!(
            decisions[..5]
                .iter()
                .all(|d| d.as_ref() == Some(&vec![extra.hash()])),
            "the agreeing replicas close in round 0"
        );
        assert_eq!(
            decisions[5], None,
            "the lagging replica cannot vote or close"
        );
        assert!(
            replicas[5].rounds[&0]
                .signers
                .get(&keys[5].public_key())
                .is_none_or(|s| s.first.is_none()),
            "it never FIRST-voted a root it does not hold"
        );
        let (decided, lagging) = (replicas[0].root(), replicas[5].root());
        assert_eq!(replicas[5].lag_reported, Some((decided, lagging)));
        // Its election reconciliation delivers the member.
        ledgers[5].record_epoch_block(0, extra.clone());
        let decisions = round(&mut replicas);
        assert_eq!(
            decisions[5],
            Some(vec![extra.hash()]),
            "it closes on the decided membership once it holds it"
        );
    }

    #[test]
    fn round_zero_is_timed_from_readiness() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        s.ready = false;
        s.enter_round(0);
        let key = PrivateKey::from(1);
        s.rounds.get_mut(&0).unwrap().started = Instant::now() - round_timeout(0) * 10;
        let (out, _) = s.drive(&[key.clone()]);
        assert!(out.is_empty(), "nothing is voted while not ready");
        assert!(
            !s.round_timer_armed,
            "the drain start does not time the round"
        );
        s.ready = true;
        let (out, _) = s.drive(&[key.clone()]);
        assert!(s.round_timer_armed, "the timer starts at readiness");
        assert!(!s.round_timed_out());
        assert!(
            out.is_empty(),
            "nothing is voted until the assembler proposes"
        );
        let root = s.root();
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
        assert!(late.is_empty(), "one FIRST vote per round");
    }

    #[test]
    fn a_replica_past_its_deadline_votes_timeout() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = PrivateKey::from(1);
        s.drive(&[key.clone()]);
        s.rounds.get_mut(&0).unwrap().started = Instant::now() - round_timeout(0);
        let root = s.root();
        propose(&mut s, BlockHash::ZERO, root);
        let (out, _) = s.drive(&[key]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![3],
            "a proposal after the deadline gets a FIRST timeout"
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
        let empty = State::new(0, RepWeights::default(), vec![]);
        assert_eq!(empty.assembler(0), None);
    }

    #[test]
    fn the_assembler_proposes_its_own_root_once_per_round() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = assembler_key(&s);
        s.ready = false;
        assert!(
            s.drive(&[key.clone()]).0.is_empty(),
            "no proposal before readiness"
        );
        s.ready = true;
        let root = s.root();
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![EpochClose::PROPOSAL, 0],
            "the assembler proposes and votes its own value"
        );
        let proposal = &out[0];
        assert!(proposal.valid_proposal());
        assert_eq!(proposal.voter, key.public_key());
        assert_eq!((proposal.parent, proposal.state), (BlockHash::ZERO, root));
        assert_eq!(out[1].candidate_id(), proposal.candidate_id());
        assert!(
            s.drive(&[key]).0.is_empty(),
            "one proposal and one FIRST vote per round"
        );
    }

    #[test]
    fn a_proposal_needs_the_round_assembler() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        let mut forged = s.template(0, BlockHash::ZERO, root);
        forged.kind = EpochClose::PROPOSAL;
        forged.sign(&PrivateKey::from(1));
        s.receive_proposal(forged);
        assert!(
            s.candidates.is_empty(),
            "key 1 is not the round-0 assembler"
        );
        let mut wrong_round = s.template(1, BlockHash::ZERO, root);
        wrong_round.kind = EpochClose::PROPOSAL;
        wrong_round.sign(&assembler_key(&s));
        s.receive_proposal(wrong_round);
        assert!(
            s.candidates.is_empty(),
            "a proposal whose round is another assembler's is ignored"
        );
        let id = propose(&mut s, BlockHash::ZERO, root).candidate_id();
        assert!(s.candidates[&id].proposal.is_some());
        assert!(s.complete(id));
    }

    #[test]
    fn a_proposal_of_another_root_is_never_voted() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let id = propose(&mut s, BlockHash::ZERO, 9.into()).candidate_id();
        assert!(
            !s.complete(id),
            "the target is not this replica's membership"
        );
        let (out, _) = s.drive(&[PrivateKey::from(1)]);
        assert!(out.is_empty());
        s.rounds.get_mut(&0).unwrap().started = Instant::now() - round_timeout(0);
        let (out, _) = s.drive(&[PrivateKey::from(1)]);
        assert_eq!(out.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn an_equivocating_assembler_stores_a_bounded_number_of_proposals() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
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
            s.retransmit
                .iter()
                .filter(|p| p.kind == EpochClose::PROPOSAL)
                .count(),
            PROPOSALS_PER_ROUND + 1,
            "held proposals are rebroadcast with the close history"
        );
    }

    #[test]
    fn a_silent_assembler_costs_one_round_and_the_next_assembler_closes() {
        let ledger = Ledger::new_null();
        let mut replicas: Vec<_> = (0..6).map(|_| ready_state(&ledger)).collect();
        let keys = sorted_keys();
        assert_eq!(replicas[1].assembler(0), Some(keys[0].public_key()));
        let mut decisions = vec![None; 6];
        for _ in 0..16 {
            let mut packets = Vec::new();
            // The round-0 assembler (replica 0) never drives.
            for i in 1..6 {
                if replicas[i].round == 0 && replicas[i].round_timer_armed {
                    replicas[i].rounds.get_mut(&0).unwrap().started =
                        Instant::now() - round_timeout(0);
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
    fn an_unready_replica_votes_nothing() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = PrivateKey::from(1);
        s.drive(&[key.clone()]);
        let root = s.root();
        propose(&mut s, BlockHash::ZERO, root);
        // A late election opens after the drain: D3 no longer holds.
        s.ready = false;
        let (out, _) = s.drive(&[key.clone()]);
        assert!(
            out.is_empty(),
            "a matching root is not voted while the replica is not drained"
        );
        s.ready = true;
        let (out, _) = s.drive(&[key]);
        assert_eq!(
            out.iter().map(|p| (p.kind, p.state)).collect::<Vec<_>>(),
            vec![(0, root)],
            "the vote follows once drained"
        );
    }

    #[test]
    fn a_member_learned_after_first_does_not_withdraw_the_vote_or_the_close() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = PrivateKey::from(1);
        s.drive(&[key.clone()]);
        let root = s.root();
        propose(&mut s, BlockHash::ZERO, root);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(out.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![0]);
        let candidate = out[0].clone();
        let id = candidate.candidate_id();
        ledger.record_epoch_block(0, rsnano_types::SavedBlock::new_test_instance().into());
        s.sync_membership(&ledger);
        assert_ne!(s.root(), root);
        assert!(s.complete(id), "the validation made at FIRST stands");
        // Notarized by certificate weight, short of a fast certificate.
        for i in 2..=4 {
            let mut vote = candidate.clone();
            vote.sign(&PrivateKey::from(i));
            s.receive(vote);
        }
        let (out, _) = s.drive(&[key.clone()]);
        assert!(
            out.iter().any(|p| p.kind == 2 && p.candidate_id() == id),
            "the FINAL vote follows the FIRST vote, not the current membership"
        );
        assert_eq!(s.round, 1, "the notarized value exits the round");
        for i in 2..=4 {
            let mut vote = candidate.clone();
            vote.sign(&PrivateKey::from(i));
            s.receive(vote);
            let mut final_vote = candidate.clone();
            final_vote.kind = 2;
            final_vote.sign(&PrivateKey::from(i));
            s.receive(final_vote);
        }
        let (_, decision) = s.drive(&[key]);
        assert_eq!(
            decision,
            Some(vec![]),
            "the epoch closes on the membership validated at FIRST, without the later member"
        );
    }

    #[test]
    fn a_lagging_replica_closes_once_its_membership_reaches_the_decided_root() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let block: rsnano_types::Block = rsnano_types::SavedBlock::new_test_instance().into();
        let decided = Ledger::epoch_state_hash(0, &[block.hash()]);
        let mut vote = s.template(0, BlockHash::ZERO, decided);
        for i in 1..=6 {
            vote.sign(&PrivateKey::from(i));
            s.receive(vote.clone());
        }
        assert!(s.has_finalized_close());
        let (out, decision) = s.drive(&[PrivateKey::from(1)]);
        assert!(out.is_empty(), "no vote after finality");
        assert_eq!(decision, None, "the decided membership is not held");
        let lagging = s.root();
        assert_eq!(s.lag_reported, Some((decided, lagging)));
        ledger.record_epoch_block(0, block.clone());
        s.sync_membership(&ledger);
        let (_, decision) = s.drive(&[PrivateKey::from(1)]);
        assert_eq!(decision, Some(vec![block.hash()]));
    }

    #[test]
    fn a_child_candidate_needs_a_notarized_parent_and_timeouts_only_between() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        let mut parent = s.template(0, BlockHash::ZERO, 77.into());
        let parent_id = parent.candidate_id();
        s.round = 2;
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
            vec![(0, parent_id)],
            "the complete child takes the FIRST vote"
        );
    }

    #[test]
    fn a_notarized_candidate_exits_the_round_without_a_timeout_certificate() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = PrivateKey::from(1);
        s.drive(&[key.clone()]);
        let root = s.root();
        propose(&mut s, BlockHash::ZERO, root);
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
        assert!(out.is_empty(), "round 1 waits for its assembler");
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
        s.advance(&[], s.weights.clone());
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
        s.advance(&hashes, s.weights.clone());
        assert_eq!(s.epoch, 1);
        let mut receipt = s.template(0, BlockHash::ZERO, digest);
        receipt.epoch = 0;
        receipt.kind = EpochClose::RECEIPT;
        receipt.members = 0;
        receipt.sign(&PrivateKey::from(1));
        s.local_receipts.push(receipt.clone());
        s.advance(&[1.into(), 2.into()], s.weights.clone());
        assert!(s.archive.iter().any(|v| v == &p));
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
            receipt.kind = EpochClose::RECEIPT;
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
                "a matching root closes on the live membership"
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
    fn readiness_waits_for_one_solicitation_of_every_overdue_election() {
        let mut s = state();
        let t0 = Instant::now();
        let threshold = Duration::from_millis(400);
        let fresh = (BlockHash::from(1), Root::from(1u64));
        let overdue = (BlockHash::from(2), Root::from(2u64));
        let listed = |fresh_open: Duration| {
            vec![
                (fresh.0, fresh.1, fresh_open),
                (overdue.0, overdue.1, Duration::from_secs(1)),
            ]
        };
        let (settled, requests) = s.settle(
            &listed(Duration::from_millis(100)),
            threshold,
            REPLY_WAIT,
            t0,
        );
        assert!(!settled, "unsettled elections keep the replica unready");
        assert_eq!(
            requests,
            vec![overdue],
            "only the election open longer than elections take to settle is solicited"
        );
        let t1 = t0 + Duration::from_millis(500);
        let (settled, requests) = s.settle(
            &listed(Duration::from_millis(600)),
            threshold,
            REPLY_WAIT,
            t1,
        );
        assert!(!settled);
        assert_eq!(
            requests,
            vec![fresh],
            "the other one once it is overdue; the first is not asked again"
        );
        let t2 = t1 + REPLY_WAIT;
        let (settled, requests) =
            s.settle(&listed(Duration::from_secs(3)), threshold, REPLY_WAIT, t2);
        assert!(settled, "every representative had time to answer both");
        assert!(requests.is_empty());
        let (settled, _) = s.settle(
            &listed(Duration::from_secs(3))[..1],
            threshold,
            REPLY_WAIT,
            t2,
        );
        assert!(settled);
        assert!(
            !s.unsettled.contains_key(&overdue.0),
            "elections no longer listed are forgotten"
        );
        assert!(
            s.settle(&[], threshold, REPLY_WAIT, t2).0,
            "nothing decided is nothing to settle"
        );
        // Once the close is due, nothing waits for the threshold, and an
        // election answered before the drain began is asked once more.
        let mut s = state();
        s.unsettled.insert(overdue.0, t0 - Duration::from_secs(5));
        s.drain_since = Some(t0 - Duration::from_secs(1));
        let (settled, requests) = s.settle(
            &listed(Duration::from_millis(100)),
            Duration::ZERO,
            DRAIN_REPLY_WAIT,
            t0,
        );
        assert!(!settled);
        assert_eq!(
            requests,
            vec![fresh, overdue],
            "every unsettled election at once"
        );
        let later = t0 + DRAIN_REPLY_WAIT;
        assert!(
            s.settle(
                &listed(Duration::from_secs(2)),
                Duration::ZERO,
                DRAIN_REPLY_WAIT,
                later
            )
            .0
        );
    }

    #[test]
    fn the_current_rounds_proposal_and_own_votes_are_refreshed() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let key = PrivateKey::from(1);
        let root = s.root();
        let proposal = propose(&mut s, BlockHash::ZERO, root);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(out.len(), 1, "the FIRST vote");
        let mut other = out[0].clone();
        other.sign(&PrivateKey::from(2));
        s.receive(other);
        let now = Instant::now();
        let refreshed = s.refresh_packets(&[key.clone()], now);
        assert_eq!(
            refreshed
                .iter()
                .map(|p| (p.kind, p.voter))
                .collect::<Vec<_>>(),
            vec![
                (EpochClose::PROPOSAL, proposal.voter),
                (0, key.public_key())
            ],
            "the proposal and this replica's own vote, not a peer's"
        );
        assert!(
            s.refresh_packets(&[key.clone()], now).is_empty(),
            "spaced by REFRESH"
        );
        assert_eq!(s.refresh_packets(&[key.clone()], now + REFRESH).len(), 2);
        s.draining = false;
        assert!(s.refresh_packets(&[key], now + REFRESH * 2).is_empty());
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

    /// The key of the current round's assembler.
    fn assembler_key(s: &State) -> PrivateKey {
        (1..=6)
            .map(PrivateKey::from)
            .find(|key| Some(key.public_key()) == s.assembler(s.round))
            .unwrap()
    }

    /// The assembler's proposal of `(parent, state)` for the current round,
    /// delivered to `s`.
    fn propose(s: &mut State, parent: BlockHash, state: BlockHash) -> EpochClose {
        let key = assembler_key(s);
        let mut proposal = s.template(s.round, parent, state);
        proposal.kind = EpochClose::PROPOSAL;
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
                if packet.kind == EpochClose::PROPOSAL {
                    replica.receive_proposal(packet.clone());
                } else {
                    replica.receive(packet.clone());
                }
            }
        }
    }
}
