//! Proposer-free Kudzu close rounds: a drained replica announces the parent
//! and membership root it would close on, and once a quorum announced the
//! same pair every replica that holds that membership FIRST-votes the value
//! it names, whether or not it is the replica's own. The announcing quorum
//! carries the D3/D4 judgment, so a member learned after the vote never
//! withdraws it. Voting epochs and canonical ledger epochs are independent.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rsnano_ledger::{AnySet, Ledger, MembershipSketch, MembershipTrie, RepWeights};
use rsnano_messages::{EpochClose, Message};
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
/// The announced value is voted once every representative announced the same
/// one, or after this long once certificate weight has: the announcement
/// phase of a round. A replica holding more members than the announced value
/// still signs it, so the wait only gives late certificates a chance to enter
/// the close instead of being discarded with it.
const AGREEMENT_TIMEOUT: Duration = Duration::from_secs(6);
static DROPPED_CLOSE_PACKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn round_timeout(round: u64) -> Duration {
    ROUND_TIMEOUT * (1u32 << round.min(6))
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
    /// Channels that delivered votes for it, asked for the sketch and pages of
    /// its view when it finalizes with a root this replica never held.
    sources: Vec<ChannelId>,
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
    parent: BlockHash,
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
    /// Every root the live membership has had: a proposal extending a parent
    /// this replica held is a superset of it, since members are only added.
    held_roots: HashSet<BlockHash>,
    closed: Option<ClosedView>,
    /// Latest readiness announcement of every representative and its channel.
    announcements: HashMap<PublicKey, (EpochClose, ChannelId)>,
    local_announcements: Vec<EpochClose>,
    announcement_seq: u64,
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
            parent: BlockHash::ZERO,
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
            held_roots: HashSet::new(),
            closed: None,
            announcements: Default::default(),
            local_announcements: Vec::new(),
            announcement_seq: 0,
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
        if !added.is_empty() || self.held_roots.is_empty() {
            let root = self.trie.root();
            self.held_roots.insert(root);
        }
        added
    }
    fn root(&mut self) -> BlockHash {
        self.trie.root()
    }

    /// The parent/root pair that certificate weight announced, if any, and
    /// whether every representative announced it. Our own keys count through
    /// our announcement only: an unready replica attests nothing, and two
    /// pairs cannot both reach certificate weight.
    fn enabled(&self) -> Option<(BlockHash, BlockHash, bool)> {
        let mut announced: HashMap<(BlockHash, BlockHash), u128> = HashMap::new();
        let mut voting = 0u128;
        for (rep, weight) in self.weights.iter() {
            if weight.is_zero() {
                continue;
            }
            voting = voting.saturating_add(weight.number());
            if let Some((announcement, _)) = self.announcements.get(rep) {
                let entry = announced
                    .entry((announcement.parent, announcement.state))
                    .or_default();
                *entry = entry.saturating_add(weight.number());
            }
        }
        let ((parent, state), weight) = announced.into_iter().max_by_key(|(_, w)| *w)?;
        (Amount::raw(weight) >= KudzuThresholds::new(self.total).certificate).then_some((
            parent,
            state,
            weight == voting,
        ))
    }
    /// Whether the announcement phase has expired since this replica became
    /// ready.
    fn agreement_expired(&self, now: Instant) -> bool {
        self.ready
            && self
                .ready_since
                .is_some_and(|since| now.duration_since(since) >= AGREEMENT_TIMEOUT)
    }
    /// The value whose FIRST vote is due: the announced pair once every
    /// representative announced it, or after `AGREEMENT_TIMEOUT` once
    /// certificate weight has.
    fn due(&self, now: Instant) -> Option<(BlockHash, BlockHash)> {
        let (parent, state, all) = self.enabled()?;
        (all || self.agreement_expired(now)).then_some((parent, state))
    }

    /// Announce the close value this replica would vote for, the current
    /// parent and live membership root, once either changes. Announcements
    /// are retransmitted with the close history until the epoch closes.
    fn announce(&mut self, keys: &[PrivateKey], out: &mut Vec<EpochClose>) {
        if !self.ready || keys.is_empty() {
            return;
        }
        let root = self.root();
        let parent = self.parent;
        if self
            .local_announcements
            .first()
            .is_some_and(|announcement| announcement.state == root && announcement.parent == parent)
        {
            return;
        }
        self.announcement_seq += 1;
        let mut announcement = self.template(self.announcement_seq, parent, root);
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
        for id in self.candidates.keys().copied().collect::<Vec<_>>() {
            let state = self.candidates[&id].header.state;
            if state != root && self.finalized(id) {
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
    /// Every round before the candidate's timed out. Any representative may
    /// vote a candidate into existence; a quorum decides, not a proposer.
    fn well_formed(&self, id: BlockHash) -> bool {
        let Some(c) = self.candidates.get(&id) else {
            return false;
        };
        (0..c.header.round).all(|r| {
            self.rounds
                .get(&r)
                .is_some_and(|x| x.certificate(BlockHash::ZERO, VoteKind::Timeout))
        })
    }
    /// A candidate carries a certificate that decides the epoch.
    fn finalized(&self, id: BlockHash) -> bool {
        self.well_formed(id)
            && (self.certified(id, VoteKind::First)
                || (self.certified(id, VoteKind::Notarize) && self.certified(id, VoteKind::Final)))
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
    /// Signable: every member of the candidate's root is held here, so its
    /// entries verify locally; and a parent, if any, is a notarized
    /// earlier-round candidate whose membership this replica held.
    fn signable(&mut self, id: BlockHash) -> bool {
        let root = self.root();
        let Some(c) = self.candidates.get(&id) else {
            return false;
        };
        let state = c.header.state;
        if !self.well_formed(id) || (state != root && !self.holds_members(state)) {
            return false;
        }
        let Some(c) = self.candidates.get(&id) else {
            return false;
        };
        if c.header.parent.is_zero() {
            return true;
        }
        let parent = c.header.parent;
        let round = c.header.round;
        self.candidates.get(&parent).is_some_and(|p| {
            p.header.round < round
                && self.certified(parent, VoteKind::Notarize)
                && self.held_roots.contains(&p.header.state)
        })
    }
    fn sign(&mut self, mut p: EpochClose, key: &PrivateKey, k: u8, out: &mut Vec<EpochClose>) {
        p.kind = k;
        p.pages = 0;
        p.page = 0;
        p.hashes.clear();
        p.sketch.clear();
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
        // A finalized candidate closes the epoch: with the live membership when
        // it is the proposed one, otherwise once its view is reconstructed.
        let root = self.root();
        let finalized: Vec<_> = self
            .candidates
            .keys()
            .copied()
            .filter(|id| self.finalized(*id))
            .collect();
        for id in finalized {
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
        // A certificate can arrive after we entered this round. Re-evaluate the
        // parent before announcing so that certificate arrival order does not
        // permanently split otherwise identical snapshots into separate chains.
        // Prefer the newest certified round, then the smallest candidate ID.
        if let Some(id) = self
            .candidates
            .iter()
            .filter(|(id, c)| {
                c.header.round < self.round
                    && self.certified(**id, VoteKind::Notarize)
                    && self.well_formed(**id)
                    && self.held_roots.contains(&c.header.state)
            })
            .min_by_key(|(id, c)| (std::cmp::Reverse(c.header.round), **id))
            .map(|(id, _)| *id)
        {
            self.parent = id;
        }
        self.announce(keys, &mut out);
        let due = self.due(now);
        // Round timeouts keep the rounds rotating even while no value has
        // enough support to be voted; reconciliation continues meanwhile.
        if !self.round_timer_armed && (due.is_some() || self.agreement_expired(now)) {
            self.arm_round_timer();
        }
        // The FIRST vote goes to the value the announcing quorum enabled,
        // whether or not it is this replica's own: replicas holding that
        // membership create the same candidate, so their votes tally without
        // a proposer, and a replica holding more members signs it as well. A
        // value the quorum did not enable is reached only through the second
        // look, so a Byzantine member cannot split the correct first votes.
        if let Some((parent, state)) = due {
            let proposal = self.template(self.round, parent, state);
            let id = proposal.candidate_id();
            self.candidates.entry(id).or_insert_with(|| Candidate {
                header: proposal.clone(),
                sources: Vec::new(),
            });
            if self.signable(id) {
                for key in keys {
                    let signer = self
                        .rounds
                        .get_mut(&self.round)
                        .unwrap()
                        .signers
                        .entry(key.public_key())
                        .or_default();
                    if signer.first.is_none() {
                        signer.first = Some(id);
                        signer.notarized.insert(id);
                        debug_trace(
                            || serde_json::json!({"type":"enabled_candidate","epoch":self.epoch,"round":self.round,"id":id,"parent":proposal.parent,"state":proposal.state,"own":proposal.state == root,"rep":key.public_key()}),
                        );
                        self.sign(proposal.clone(), key, 0, &mut out);
                    }
                }
            }
        }
        let current: Vec<_> = self
            .candidates
            .iter()
            .filter(|(_, c)| c.header.round == self.round)
            .map(|(id, c)| (*id, c.header.clone()))
            .collect();
        for (id, p) in &current {
            // Notarizing another value on second look needs its members held
            // here. A FINAL vote is bound to the FIRST vote only: the
            // authorization a quorum gave that value is not withdrawn by a
            // member this replica learned since.
            let signable = self.signable(*id);
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
        // A new close round requires a timeout certificate. This ensures no
        // earlier round can later finalize a different target.
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
                    "parent":state.parent,
                    "ready_for_ms":state.ready_since.map(|since| since.elapsed().as_millis()),
                    "enabled":enabled.map(|(parent, state, all)| serde_json::json!({"parent":parent,"state":state,"all":all})),
                    "announcements":state.announcements.iter().map(|(rep, (a, _))| serde_json::json!({"rep":rep,"members":a.members,"parent":a.parent,"state":a.state})).collect::<Vec<_>>(),
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
                eprintln!(
                    "EPOCH_CLOSED {}",
                    serde_json::json!({"pid":std::process::id(),"unix_us":closed_unix_us,"epoch":state.epoch,"hash":digest,"blocks":hashes.len(),"round":state.round,"discarded":discarded})
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
    fn leaderless_close_with_one_silent_member() {
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
            "five identical candidates certify in round 0 without a proposer"
        );
        assert_eq!(
            replicas[0].candidates.len(),
            1,
            "replicas with the same membership and parent create the same candidate"
        );
    }

    #[test]
    fn first_proposal_waits_until_every_representative_announces_the_same_root() {
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
        for _ in 0..4 {
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
        assert!(
            s.round_timer_armed,
            "the timer starts when the vote becomes due"
        );
        assert!(!s.round_timed_out(), "and not from the stale drain start");
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![0],
            "the replica votes FIRST for its own announced root"
        );
        assert_eq!(out[0].state, s.root());
        s.rounds.get_mut(&0).unwrap().started = Instant::now() - round_timeout(0);
        assert!(s.round_timed_out());
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
    fn the_enabled_value_needs_certificate_weight_and_counts_only_announcements() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        assert_eq!(s.enabled(), None);
        let mut announcement = s.template(1, BlockHash::ZERO, root);
        announcement.kind = 8;
        announcement.set_sketch(&s.trie.sketch().to_bytes());
        for i in 2..=4 {
            announcement.sign(&PrivateKey::from(i));
            s.receive_announcement(announcement.clone(), ChannelId::from(i as usize));
        }
        assert_eq!(
            s.enabled(),
            None,
            "three of six are below certificate weight"
        );
        announcement.sign(&PrivateKey::from(5));
        s.receive_announcement(announcement.clone(), ChannelId::from(5));
        assert_eq!(s.enabled(), Some((BlockHash::ZERO, root, false)));
        assert_eq!(s.due(Instant::now()), None);
        s.ready_since = Some(Instant::now() - AGREEMENT_TIMEOUT);
        assert_eq!(
            s.due(Instant::now()),
            Some((BlockHash::ZERO, root)),
            "certificate weight suffices after the announcement phase"
        );
        let mut stale = announcement.clone();
        stale.state = 9.into();
        stale.round = 0;
        stale.sign(&PrivateKey::from(6));
        s.receive_announcement(stale, ChannelId::from(6));
        announcement.sign(&PrivateKey::from(6));
        s.receive_announcement(announcement.clone(), ChannelId::from(6));
        assert_eq!(
            s.enabled(),
            Some((BlockHash::ZERO, root, false)),
            "our own weight counts only through our announcement"
        );
        let mut out = Vec::new();
        s.announce(&[PrivateKey::from(1)], &mut out);
        assert_eq!(s.enabled(), Some((BlockHash::ZERO, root, true)));
        let mut older = announcement.clone();
        older.state = 9.into();
        older.round = 0;
        older.sign(&PrivateKey::from(6));
        s.receive_announcement(older, ChannelId::from(6));
        assert_eq!(
            s.enabled().map(|(_, _, all)| all),
            Some(true),
            "an older sequence does not replace a newer one"
        );
        s.ready_since = None;
        assert_eq!(s.due(Instant::now()), Some((BlockHash::ZERO, root)));
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
        assert!(out.is_empty(), "an unsupported membership is not proposed");
        assert!(s.round_timer_armed, "but the round is timed from here");
        s.rounds.get_mut(&0).unwrap().started = Instant::now() - round_timeout(0);
        let (out, _) = s.drive(&[key]);
        assert_eq!(out.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn a_changed_membership_is_announced_instead_of_voted() {
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
        // A member arrives before the vote is cast.
        ledger.record_epoch_block(0, rsnano_types::SavedBlock::new_test_instance().into());
        s.sync_membership(&ledger);
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(
            out.iter().map(|p| p.kind).collect::<Vec<_>>(),
            vec![8],
            "the new root is announced, nothing voted"
        );
        assert!(s.candidates.is_empty());
        assert_eq!(out[0].members, 1);
        for i in 1..=6 {
            let mut announcement = out[0].clone();
            announcement.sign(&PrivateKey::from(i));
            s.receive_announcement(announcement, ChannelId::from(i as usize));
        }
        let (out, _) = s.drive(&[key]);
        assert_eq!(out.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![0]);
        assert_eq!(out[0].state, s.root());
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
        for i in 2..=6 {
            let mut announcement = announced.clone();
            announcement.sign(&PrivateKey::from(i));
            s.receive_announcement(announcement, ChannelId::from(i as usize));
        }
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
    fn a_candidate_nobody_announced_is_unsignable_once_the_membership_grows() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        let mut p = s.template(0, BlockHash::ZERO, root);
        p.sign(&PrivateKey::from(2));
        let id = p.candidate_id();
        s.receive(p);
        assert!(s.signable(id));
        let block: rsnano_types::Block = rsnano_types::SavedBlock::new_test_instance().into();
        ledger.record_epoch_block(0, block);
        s.sync_membership(&ledger);
        assert!(
            !s.signable(id),
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
        assert_eq!(mine.enabled(), Some((BlockHash::ZERO, their_root, false)));
        mine.ready_since = Some(Instant::now() - AGREEMENT_TIMEOUT);
        let (out, _) = mine.drive(&[PrivateKey::from(1)]);
        assert_eq!(
            out.iter().map(|p| (p.kind, p.state)).collect::<Vec<_>>(),
            vec![(0, their_root)],
            "the FIRST vote adopts the enabled value that omits a member held here"
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
        for i in 1..=6 {
            let mut announcement = announced.clone();
            announcement.sign(&PrivateKey::from(i));
            s.receive_announcement(announcement, ChannelId::from(i as usize));
        }
        let (out, _) = s.drive(&[key.clone()]);
        assert_eq!(out.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![0]);
        let candidate = out[0].clone();
        ledger.record_epoch_block(0, rsnano_types::SavedBlock::new_test_instance().into());
        s.sync_membership(&ledger);
        assert!(!s.signable(candidate.candidate_id()));
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
    fn a_child_candidate_needs_a_parent_this_replica_held() {
        let ledger = Ledger::new_null();
        let mut s = ready_state(&ledger);
        let root = s.root();
        let mut parent = s.template(0, BlockHash::ZERO, 77.into());
        parent.sign(&PrivateKey::from(1));
        let parent_id = parent.candidate_id();
        // Notarized by certificate weight, short of a fast certificate.
        for i in 1..=6 {
            if i <= 4 {
                parent.sign(&PrivateKey::from(i));
                s.receive(parent.clone());
            }
            let mut timeout = s.template(0, BlockHash::ZERO, BlockHash::ZERO);
            timeout.kind = 4;
            timeout.sign(&PrivateKey::from(i));
            s.receive(timeout);
        }
        s.round = 1;
        let mut child = s.template(1, parent_id, root);
        child.sign(&PrivateKey::from(2));
        let child_id = child.candidate_id();
        s.receive(child);
        assert!(
            !s.signable(child_id),
            "the parent membership was never held here"
        );
        s.held_roots.insert(77.into());
        assert!(s.signable(child_id));
        s.enter_round(1);
        let (out, _) = s.drive(&[PrivateKey::from(1)]);
        assert_eq!(
            out.iter().map(|p| (p.kind, p.parent)).collect::<Vec<_>>(),
            vec![(8, parent_id)],
            "the announcement names the notarized parent this replica extends"
        );
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
        assert_eq!(round_timeout(u64::MAX), Duration::from_secs(192));
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
                } else {
                    replica.receive(packet.clone());
                }
            }
        }
    }
}
