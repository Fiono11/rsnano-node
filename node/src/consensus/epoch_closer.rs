//! Priority Kudzu close rounds. Voting epochs and canonical ledger epochs are independent.
use crate::{
    consensus::{AecService, VoteGenerators, election::kudzu::KudzuVotes},
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};
use rsnano_ledger::{AnySet, Ledger, RepWeights};
use rsnano_messages::{EpochClose, Message};
use rsnano_network::{ChannelId, TrafficType};
use rsnano_types::{Amount, BlockHash, PrivateKey, PublicKey, Root, Signature, Vote, VoteKind};
use rsnano_utils::{CancellationToken, ticker::Tickable};
use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

const ROUND_TIMEOUT: Duration = Duration::from_secs(3);
const RETRANSMIT: Duration = Duration::from_secs(2);
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
    validated: Cell<bool>,
    header: EpochClose,
    pages: BTreeMap<u16, Vec<BlockHash>>,
    removed_pages: BTreeMap<u16, Vec<BlockHash>>,
    recovery_base: Option<Arc<Vec<BlockHash>>>,
    last_delta_page: Option<Instant>,
    hashes: Option<Vec<BlockHash>>,
}
struct State {
    cut: super::epoch_cut::EpochCut,
    epoch: u64,
    round: u64,
    parent: BlockHash,
    ready: bool,
    rounds: BTreeMap<u64, Round>,
    candidates: BTreeMap<BlockHash, Candidate>,
    weights: RepWeights,
    total: Amount,
    last_send: Instant,
    archive: Vec<EpochClose>,
    receipts: BTreeMap<(u64, PublicKey), BlockHash>,
    local_receipts: Vec<EpochClose>,
    local_snapshots: VecDeque<(BlockHash, Vec<BlockHash>)>,
    last_snapshot: Option<Instant>,
    archive_snapshots: BTreeMap<BlockHash, Vec<BlockHash>>,
    closed_base: Option<(BlockHash, Vec<BlockHash>)>,
    deltas: VecDeque<((BlockHash, BlockHash), SnapshotDelta)>,
    retransmit: VecDeque<EpochClose>,
    solicited: HashMap<BlockHash, Instant>,
    last_reconcile: Option<Instant>,
    last_timeout_solicitation: Option<Instant>,
    /// Largest member count carried by any accepted vote of this epoch.
    highest_members: u64,
    /// Local member count when the current round started.
    round_start_members: u64,
    /// Position in the membership recovery targets for the next solicitation.
    recovery_cursor: usize,
}
fn sorted_difference(
    before: &[BlockHash],
    after: &[BlockHash],
) -> (Vec<BlockHash>, Vec<BlockHash>) {
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < before.len() || j < after.len() {
        match (before.get(i), after.get(j)) {
            (Some(b), Some(a)) if b == a => {
                i += 1;
                j += 1;
            }
            (Some(b), Some(a)) if b < a => {
                removed.push(*b);
                i += 1;
            }
            (Some(_), Some(a)) => {
                added.push(*a);
                j += 1;
            }
            (Some(b), None) => {
                removed.push(*b);
                i += 1;
            }
            (None, Some(a)) => {
                added.push(*a);
                j += 1;
            }
            (None, None) => break,
        }
    }
    (added, removed)
}
struct SnapshotDelta {
    added: Vec<BlockHash>,
    removed: Vec<BlockHash>,
}
impl SnapshotDelta {
    fn between(base: &[BlockHash], target: &[BlockHash]) -> Self {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let (mut b, mut t) = (0, 0);
        while b < base.len() && t < target.len() {
            match base[b].cmp(&target[t]) {
                std::cmp::Ordering::Less => {
                    removed.push(base[b]);
                    b += 1;
                }
                std::cmp::Ordering::Greater => {
                    added.push(target[t]);
                    t += 1;
                }
                std::cmp::Ordering::Equal => {
                    b += 1;
                    t += 1;
                }
            }
        }
        removed.extend_from_slice(&base[b..]);
        added.extend_from_slice(&target[t..]);
        Self { added, removed }
    }
}
impl State {
    fn new(epoch: u64, weights: RepWeights, archive: Vec<EpochClose>) -> Self {
        let total = Amount::raw(
            weights
                .values()
                .fold(0u128, |s, w| s.saturating_add(w.number())),
        );
        Self {
            cut: Default::default(),
            epoch,
            round: 0,
            parent: BlockHash::ZERO,
            ready: false,
            rounds: Default::default(),
            candidates: Default::default(),
            weights,
            total,
            last_send: Instant::now() - RETRANSMIT,
            archive,
            receipts: BTreeMap::new(),
            local_receipts: Vec::new(),
            local_snapshots: Default::default(),
            last_snapshot: None,
            archive_snapshots: Default::default(),
            closed_base: None,
            deltas: Default::default(),
            retransmit: Default::default(),
            solicited: Default::default(),
            last_reconcile: None,
            last_timeout_solicitation: None,
            highest_members: 0,
            round_start_members: 0,
            recovery_cursor: 0,
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
    /// Size of the latest local snapshot.
    fn members(&self) -> u64 {
        self.local_snapshots
            .back()
            .map(|(_, hashes)| hashes.len() as u64)
            .unwrap_or(0)
    }
    /// Whether a timed-out round may be left for the next one. A replica behind
    /// the most advanced proposal keeps recovering instead: a round it starts
    /// could not succeed. A replica that caught up starts the next round; the
    /// replica already at the highest count joins once more than `f` weight has
    /// started it, or once the round timer expires, which keeps the rounds live.
    fn may_start_next_round(&self) -> bool {
        let members = self.members();
        if members < self.highest_members {
            return false;
        }
        let started = self
            .rounds
            .get(&(self.round + 1))
            .is_some_and(|round| round.tally.has_f_plus_one_first_votes());
        let progressed = members > self.round_start_members;
        let timed = self
            .rounds
            .get(&self.round)
            .is_some_and(|round| round.started.elapsed() >= round_timeout(self.round));
        started || progressed || timed
    }
    fn enter_round(&mut self, round: u64) {
        self.round = round;
        self.rounds.entry(round).or_default().started = Instant::now();
        self.round_start_members = self.members();
    }
    /// Queue everything a lagging peer may still need, newest rounds first so the
    /// votes that decide the current round are delivered before older history.
    /// A cycle still in progress is finished before a new one starts, so every
    /// packet is eventually sent regardless of archive size.
    fn queue_retransmission(&mut self) {
        if !self.retransmit.is_empty() {
            return;
        }
        self.retransmit.extend(self.cut.local.iter().cloned());
        self.retransmit.extend(self.local_receipts.iter().cloned());
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
    fn advance(&mut self, hashes: Vec<BlockHash>, weights: RepWeights) {
        let mut archive = std::mem::take(&mut self.archive);
        archive.extend(self.packets());
        archive.extend(self.cut.local.iter().cloned());
        let mut archive_snapshots = std::mem::take(&mut self.archive_snapshots);
        archive_snapshots.extend(
            self.candidates
                .values()
                .filter_map(|c| c.hashes.as_ref().map(|h| (c.header.state, h.clone())))
                .chain(self.local_snapshots.iter().cloned()),
        );
        if let Some((digest, hashes)) = &self.closed_base {
            archive_snapshots.insert(*digest, hashes.clone());
        }
        let receipts = std::mem::take(&mut self.receipts);
        let local_receipts = std::mem::take(&mut self.local_receipts);
        *self = State::new(self.epoch + 1, weights, archive);
        self.archive_snapshots = archive_snapshots;
        self.closed_base = Some((Ledger::epoch_state_hash(&hashes), hashes));
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
            state,
            kind: 0,
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            page: 0,
            pages: 0,
            hashes: vec![],
            base: BlockHash::ZERO,
            removed: vec![],
            members: self.members(),
        }
    }
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
        if p.epoch != self.epoch || p.round > self.round + FUTURE_ROUNDS {
            return;
        }
        if p.kind <= 4 {
            if self.weights.weight(&p.voter).is_zero() || !p.valid_vote() {
                return;
            }
            let accepted = self.rounds.entry(p.round).or_default().receive(
                p.clone(),
                &self.weights,
                self.total,
            );
            if accepted {
                self.highest_members = self.highest_members.max(p.members);
            }
            if accepted && p.kind <= 2 && !p.state.is_zero() {
                self.candidates
                    .entry(p.candidate_id())
                    .or_insert_with(|| Candidate {
                        validated: Cell::new(false),
                        header: p.clone(),
                        pages: Default::default(),
                        removed_pages: Default::default(),
                        recovery_base: None,
                        last_delta_page: None,
                        hashes: None,
                    });
            }
        } else if p.kind == 5 && p.pages > 0 && p.page < p.pages && !p.base.is_zero() {
            // A delta is usable only against an immutable snapshot we already
            // reconstructed. There is deliberately no full-list fallback.
            let Some(base) = self
                .candidates
                .get(&p.candidate_id())
                .filter(|c| c.header.base == p.base)
                .and_then(|c| c.recovery_base.clone())
                .or_else(|| self.snapshot(&p.base).cloned().map(Arc::new))
            else {
                return;
            };
            let id = p.candidate_id();
            // Bound simultaneous payload assemblies, not authenticated decision metadata.
            if self
                .candidates
                .get(&id)
                .is_some_and(|c| c.header.pages == 0)
                && self
                    .candidates
                    .values()
                    .filter(|c| c.hashes.is_none() && c.recovery_base.is_some())
                    .count()
                    >= 64
            {
                return;
            }
            let Some(entry) = self.candidates.get_mut(&id) else {
                return;
            };
            if entry.hashes.is_some() {
                return;
            }
            if entry.header.pages == 0 {
                entry.header.pages = p.pages;
                entry.header.base = p.base;
                entry.recovery_base = Some(base.clone());
            }
            if entry.header.pages != p.pages || entry.header.base != p.base {
                return;
            }
            if !entry.pages.contains_key(&p.page) {
                entry.last_delta_page = Some(Instant::now());
            }
            entry.pages.entry(p.page).or_insert(p.hashes);
            entry.removed_pages.entry(p.page).or_insert(p.removed);
            if entry.pages.len() == p.pages as usize {
                let added: Vec<_> = entry.pages.values().flatten().copied().collect();
                let removed: Vec<_> = entry.removed_pages.values().flatten().copied().collect();
                let valid = added.windows(2).all(|w| w[0] < w[1])
                    && removed.windows(2).all(|w| w[0] < w[1])
                    && added.iter().all(|h| base.binary_search(h).is_err())
                    && removed.iter().all(|h| base.binary_search(h).is_ok());
                let mut reconstructed: std::collections::BTreeSet<_> =
                    base.iter().copied().collect();
                for hash in removed {
                    reconstructed.remove(&hash);
                }
                reconstructed.extend(added);
                let hashes: Vec<_> = reconstructed.into_iter().collect();
                if valid && Ledger::epoch_state_hash(&hashes) == p.state {
                    entry.hashes = Some(hashes);
                } else {
                    entry.header.pages = 0;
                    entry.header.base = BlockHash::ZERO;
                }
                entry.recovery_base = None;
                entry.pages.clear();
                entry.removed_pages.clear();
            }
        }
    }

    fn certified(&self, id: BlockHash, k: VoteKind) -> bool {
        self.candidates.get(&id).is_some_and(|c| {
            self.rounds
                .get(&c.header.round)
                .is_some_and(|r| r.certificate(id, k))
        })
    }
    fn valid(&self, id: BlockHash, ledger: &Ledger) -> bool {
        let Some(c) = self.candidates.get(&id) else {
            return false;
        };
        if c.validated.get() {
            return true;
        }
        let Some(hashes) = &c.hashes else {
            return false;
        };
        if !ledger.epoch_close_candidate_valid(self.epoch, hashes) {
            return false;
        }
        let first_skipped = if c.header.parent.is_zero() {
            0
        } else {
            let Some(parent) = self.candidates.get(&c.header.parent) else {
                return false;
            };
            if parent.header.round >= c.header.round
                || !self.certified(c.header.parent, VoteKind::Notarize)
                || !self.valid(c.header.parent, ledger)
            {
                return false;
            }
            let Some(previous) = &parent.hashes else {
                return false;
            };
            if !previous.iter().all(|h| hashes.binary_search(h).is_ok()) {
                return false;
            }
            parent.header.round + 1
        };
        let valid = (first_skipped..c.header.round).all(|r| {
            self.rounds
                .get(&r)
                .is_some_and(|x| x.certificate(BlockHash::ZERO, VoteKind::Timeout))
        });
        c.validated.set(valid);
        valid
    }
    fn sign(&mut self, mut p: EpochClose, key: &PrivateKey, k: u8, out: &mut Vec<EpochClose>) {
        p.kind = k;
        p.pages = 0;
        p.page = 0;
        p.hashes.clear();
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
    /// Proposals and votes never include snapshot members or deltas.
    fn packets(&self) -> Vec<EpochClose> {
        self.rounds
            .values()
            .flat_map(|r| r.votes.values().cloned())
            .collect()
    }

    fn snapshot(&self, digest: &BlockHash) -> Option<&Vec<BlockHash>> {
        self.local_snapshots
            .iter()
            .find(|(h, _)| h == digest)
            .map(|(_, v)| v)
            .or_else(|| {
                self.candidates
                    .values()
                    .find(|c| c.header.state == *digest && c.hashes.is_some())
                    .and_then(|c| c.hashes.as_ref())
            })
            .or_else(|| self.archive_snapshots.get(digest))
            .or_else(|| {
                self.closed_base
                    .as_ref()
                    .filter(|(h, _)| h == digest)
                    .map(|(_, v)| v)
            })
    }

    fn bases(&self) -> Vec<BlockHash> {
        let mut bases: std::collections::BTreeSet<_> = self
            .local_snapshots
            .iter()
            .filter(|(_, hashes)| !hashes.is_empty())
            .map(|(h, _)| *h)
            .collect();
        bases.extend(
            self.candidates
                .values()
                .filter(|c| c.hashes.as_ref().is_some_and(|v| !v.is_empty()))
                .map(|c| c.header.state),
        );
        bases.extend(
            self.archive_snapshots
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(h, _)| *h),
        );
        if let Some((digest, _)) = &self.closed_base {
            bases.insert(*digest);
        }
        bases.into_iter().take(EpochClose::PAGE_SIZE).collect()
    }

    fn recovery_page(&mut self, request: &EpochClose) -> Option<EpochClose> {
        if !request.valid_recovery_request() || self.weights.weight(&request.voter).is_zero() {
            return None;
        }
        let cached = self
            .deltas
            .iter()
            .find(|((target, base), _)| *target == request.state && request.hashes.contains(base))
            .map(|(key, _)| *key);
        // Prefer the largest shared base. A retry after the first delta page
        // advertises only the selected base, keeping the transfer immutable.
        let cache_key = if let Some(key) = cached {
            key
        } else {
            let target = self.snapshot(&request.state)?;
            let base = request
                .hashes
                .iter()
                .filter_map(|h| self.snapshot(h).map(|v| (*h, v)))
                .filter(|(_, v)| !v.is_empty())
                .max_by_key(|(_, v)| v.len());
            let (base_hash, base) = base?;
            let cache_key = (request.state, base_hash);
            let delta = SnapshotDelta::between(base, target);
            if self.deltas.len() == 16 {
                self.deltas.pop_front();
            }
            self.deltas.push_back((cache_key, delta));
            cache_key
        };
        let delta = &self.deltas.iter().find(|(key, _)| *key == cache_key)?.1;
        let count = delta.added.len() + delta.removed.len();
        let pages = count.div_ceil(EpochClose::PAGE_SIZE).max(1);
        if pages > EpochClose::MAX_PAGES as usize || request.page as usize >= pages {
            return None;
        }
        let start = request.page as usize * EpochClose::PAGE_SIZE;
        let end = (start + EpochClose::PAGE_SIZE).min(count);
        let mut page = request.clone();
        page.kind = 5;
        page.base = cache_key.1;
        page.pages = pages as u16;
        page.hashes =
            delta.added[start.min(delta.added.len())..end.min(delta.added.len())].to_vec();
        page.removed = delta.removed
            [start.saturating_sub(delta.added.len())..end.saturating_sub(delta.added.len())]
            .to_vec();
        Some(page)
    }
    /// Members of a reconstructed peer snapshot that carry no local membership in
    /// this epoch. Their notarizations must be learned before the snapshot can be
    /// validated, and their elections may already be timed out locally and no
    /// longer solicit on their own, so the closer requests them directly. Roots
    /// unknown locally are left zero; the block tree of the peer resolves them.
    fn reconciliation_targets(&mut self, ledger: &Ledger) -> Vec<(BlockHash, Root)> {
        let now = Instant::now();
        if self
            .last_reconcile
            .is_some_and(|last| now.duration_since(last) < RETRANSMIT)
        {
            return Vec::new();
        }
        self.last_reconcile = Some(now);
        let complete: Vec<_> = self
            .candidates
            .iter()
            .filter(|(_, c)| c.hashes.is_some())
            .map(|(id, _)| *id)
            .collect();
        let mut targets = Vec::new();
        for id in complete {
            if self.valid(id, ledger) {
                continue;
            }
            let missing = {
                let hashes = self.candidates[&id].hashes.as_ref().unwrap();
                ledger.epoch_close_missing_members(self.epoch, hashes)
            };
            for hash in missing {
                if targets.len() == rsnano_messages::ConfirmReq::HASHES_MAX {
                    return targets;
                }
                if !self.solicitation_due(hash, now) {
                    continue;
                }
                let root = ledger
                    .any()
                    .get_block(&hash)
                    .map(|block| block.root())
                    .unwrap_or_else(|| Root::from(0u64));
                targets.push((hash, root));
            }
        }
        targets
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
    fn drive(
        &mut self,
        ledger: &Ledger,
        keys: &[PrivateKey],
    ) -> (Vec<EpochClose>, Option<Vec<BlockHash>>) {
        let mut out = Vec::new();
        for candidate in self.candidates.values_mut().filter(|c| c.hashes.is_none()) {
            if candidate
                .last_delta_page
                .is_some_and(|t| t.elapsed() >= RETRANSMIT * 5)
            {
                // The serving peer may have evicted its bounded base/delta cache.
                // Retry with current bases instead of pinning an unavailable base forever.
                candidate.header.base = BlockHash::ZERO;
                candidate.header.pages = 0;
                candidate.pages.clear();
                candidate.removed_pages.clear();
                candidate.recovery_base = None;
                candidate.last_delta_page = None;
            }
        }
        // A compact vote is sufficient when its digest is already reconstructible
        // locally. Only a mismatch needs separate snapshot recovery.
        let propose = self.ready
            && keys.iter().any(|key| {
                !self.weights.weight(&key.public_key()).is_zero()
                    && self
                        .rounds
                        .get(&self.round)
                        .and_then(|round| round.signers.get(&key.public_key()))
                        .is_none_or(|signer| signer.first.is_none())
            });
        // A signed FIRST cannot change within its round. Rebuild immediately for
        // a new proposal; between proposals refresh bases only as often as they
        // can be advertised/requested, rather than scanning the ledger each tick.
        let refresh = propose
            || ((self.ready || self.candidates.values().any(|c| c.hashes.is_none()))
                && self
                    .last_snapshot
                    .is_none_or(|last| last.elapsed() >= RETRANSMIT));
        let local = refresh.then(|| {
            let hashes = ledger.epoch_close_candidate(self.epoch);
            self.last_snapshot = Some(Instant::now());
            hashes
        });
        if let Some(hashes) = &local {
            let digest = Ledger::epoch_state_hash(hashes);
            if !self.local_snapshots.iter().any(|(h, _)| *h == digest) {
                if self.ready {
                    if let Some((_, previous)) = self.local_snapshots.back() {
                        let (added, removed) = sorted_difference(previous, hashes);
                        eprintln!(
                            "EPOCH_MEMBERS_CHANGED {}",
                            serde_json::json!({
                                "pid":std::process::id(),"epoch":self.epoch,"round":self.round,"count":hashes.len(),"state":digest,
                                "added":added.iter().take(8).collect::<Vec<_>>(),"removed":removed.iter().take(8).collect::<Vec<_>>(),
                                "added_total":added.len(),"removed_total":removed.len()
                            })
                        );
                    }
                }
                if self.local_snapshots.len() == 8 {
                    self.local_snapshots.pop_front();
                }
                self.local_snapshots.push_back((digest, hashes.clone()));
            }
            let missing: Vec<_> = self
                .candidates
                .iter()
                .filter(|(_, c)| c.hashes.is_none())
                .map(|(id, c)| (*id, c.header.state))
                .collect();
            for (id, state) in missing {
                let matching = if state == digest {
                    Some(hashes.clone())
                } else {
                    self.snapshot(&state).cloned()
                };
                if let Some(hashes) = matching {
                    let candidate = self.candidates.get_mut(&id).unwrap();
                    candidate.hashes = Some(hashes);
                    candidate.pages.clear();
                    candidate.removed_pages.clear();
                    candidate.recovery_base = None;
                }
            }
        }
        // A later certified block finalizes its ancestors. The first block in that
        // committed chain is the unique Close(e); later snapshots are successors,
        // never alternative decisions that replace an already closed epoch.
        for (&id, c) in &self.candidates {
            if self.valid(id, ledger)
                && (self.certified(id, VoteKind::First)
                    || (self.certified(id, VoteKind::Notarize)
                        && self.certified(id, VoteKind::Final)))
            {
                let mut root = c;
                while !root.header.parent.is_zero() {
                    let Some(parent) = self.candidates.get(&root.header.parent) else {
                        return (out, None);
                    };
                    root = parent;
                }
                debug_trace(
                    || serde_json::json!({"type":"decision","epoch":self.epoch,"certified":id,"selected":root.header.candidate_id(),"fast":self.certified(id,VoteKind::First),"candidates":self.candidates.iter().map(|(id,c)| serde_json::json!({"id":id,"round":c.header.round,"parent":c.header.parent,"state":c.header.state,"hashes":c.hashes})).collect::<Vec<_>>(),"votes":self.rounds.iter().map(|(r,v)| (*r,v.votes.values().map(|p| serde_json::json!({"voter":p.voter,"kind":p.kind,"id":p.candidate_id()})).collect::<Vec<_>>())).collect::<BTreeMap<_,_>>() }),
                );
                return (out, root.hashes.clone());
            }
        }
        if !self.ready {
            return (out, None);
        }
        self.rounds.entry(self.round).or_default();
        // A certificate can arrive after we entered this round. Re-evaluate the
        // parent before proposing so that certificate arrival order does not
        // permanently split otherwise identical snapshots into separate chains.
        // Prefer the newest certified round, then the smallest candidate ID.
        if let Some((id, _)) = self
            .candidates
            .iter()
            .filter(|(id, c)| {
                c.header.round < self.round
                    && self.certified(**id, VoteKind::Notarize)
                    && self.valid(**id, ledger)
            })
            .min_by_key(|(id, c)| (std::cmp::Reverse(c.header.round), **id))
        {
            self.parent = *id;
        }
        // Every representative proposes its current monotonic epoch snapshot.
        // Keep recent bases for recovery, but create only signed proposals.
        // FIRST remains immutable; the next round uses the latest snapshot.
        if propose {
            let hashes = local.unwrap();
            if hashes.len() <= EpochClose::PAGE_SIZE * EpochClose::MAX_PAGES as usize {
                let proposal =
                    self.template(self.round, self.parent, Ledger::epoch_state_hash(&hashes));
                let id = proposal.candidate_id();
                self.candidates.entry(id).or_insert_with(|| Candidate {
                    validated: Cell::new(false),
                    header: proposal.clone(),
                    pages: Default::default(),
                    removed_pages: Default::default(),
                    recovery_base: None,
                    last_delta_page: None,
                    hashes: Some(hashes),
                });
                if self.valid(id, ledger) {
                    for key in keys {
                        if self.weights.weight(&key.public_key()).is_zero() {
                            continue;
                        }
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
                                || serde_json::json!({"type":"proposal","epoch":self.epoch,"round":self.round,"id":id,"parent":proposal.parent,"state":proposal.state,"hashes":self.candidates[&id].hashes,"rep":key.public_key()}),
                            );
                            self.sign(proposal.clone(), key, 0, &mut out);
                        }
                    }
                }
            }
        }
        let valid: Vec<_> = self
            .candidates
            .iter()
            .filter(|(id, c)| c.header.round == self.round && self.valid(**id, ledger))
            .map(|(id, c)| (*id, c.header.clone()))
            .collect();
        for (id, p) in &valid {
            for key in keys {
                let r = self.rounds.get_mut(&self.round).unwrap();
                let second = r.tally.second_look(id);
                let notarized = r.certificate(*id, VoteKind::Notarize);
                let signer = r.signers.entry(key.public_key()).or_default();
                let mut action = None;
                if second
                    && signer.first.is_some()
                    && !signer.notarized.contains(id)
                    && signer.final_vote.is_none()
                    && signer.notarized.len() < 3
                {
                    signer.notarized.insert(*id);
                    action = Some(1);
                } else if notarized
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
        // Enter the next slot on a notarized block, or on a timeout certificate.
        if let Some((id, _)) = valid
            .iter()
            .find(|(id, _)| self.certified(*id, VoteKind::Notarize))
        {
            self.parent = *id;
            self.enter_round(self.round + 1);
            return (out, None);
        }
        let p = self.template(self.round, BlockHash::ZERO, BlockHash::ZERO);
        for key in keys {
            let r = self.rounds.get_mut(&self.round).unwrap();
            let timed = r.started.elapsed() >= round_timeout(self.round);
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
        if self.rounds[&self.round].certificate(BlockHash::ZERO, VoteKind::Timeout)
            && self.may_start_next_round()
        {
            let expired = self.round;
            self.enter_round(self.round + 1);
            // Retain certified ancestors; discard unsuccessful snapshot payloads.
            self.candidates.retain(|_, c| c.header.round != expired);
        }
        (out, None)
    }
}

#[derive(Default)]
struct Recovery {
    sources: HashMap<BlockHash, Vec<ChannelId>>,
    requested: HashMap<(BlockHash, u16), Instant>,
    replies: VecDeque<(ChannelId, EpochClose)>,
    next_source: usize,
}

impl Recovery {
    fn requests(&mut self, state: &State, key: &PrivateKey) -> Vec<(ChannelId, EpochClose)> {
        self.sources
            .retain(|id, _| state.candidates.contains_key(id));
        self.requested
            .retain(|(id, _), _| state.candidates.get(id).is_some_and(|c| c.hashes.is_none()));
        let bases = state.bases();
        let mut result = Vec::new();
        let now = Instant::now();
        for (id, candidate) in &state.candidates {
            if candidate.hashes.is_some() {
                continue;
            }
            let Some(sources) = self.sources.get(id).filter(|v| !v.is_empty()) else {
                continue;
            };
            let bases = if candidate.header.base.is_zero() {
                bases.clone()
            } else {
                vec![candidate.header.base]
            };
            if bases.is_empty() {
                continue;
            }
            for page in 0..candidate.header.pages.max(1) {
                if candidate.pages.contains_key(&page)
                    || self
                        .requested
                        .get(&(*id, page))
                        .is_some_and(|t| now.duration_since(*t) < RETRANSMIT)
                {
                    continue;
                }
                let mut request = candidate.header.clone();
                request.kind = 7;
                request.page = page;
                request.pages = 0;
                request.base = BlockHash::ZERO;
                request.hashes = bases.clone();
                request.removed.clear();
                request.sign(key);
                let channel = sources[self.next_source % sources.len()];
                self.next_source = self.next_source.wrapping_add(1);
                self.requested.insert((*id, page), now);
                result.push((channel, request));
                if result.len() == 32 {
                    return result;
                }
            }
        }
        result
    }
}

pub(crate) struct EpochCloser {
    ledger: Arc<Ledger>,
    generators: Arc<VoteGenerators>,
    aec: Arc<AecService>,
    reps: Arc<Mutex<WalletRepresentatives>>,
    flooder: Mutex<MessageFlooder>,
    incoming: Mutex<VecDeque<(EpochClose, ChannelId)>>,
    recovery: Mutex<Recovery>,
    state: Mutex<State>,
    epoch_start: Mutex<Option<Instant>>,
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
        let state = State::new(
            ledger.closed_epoch_count.load(Ordering::Acquire),
            ledger.rep_weights.read().clone(),
            vec![],
        );
        let epoch_start_file =
            std::env::var_os("NANOSPAM_RAI_EPOCH_START_FILE").map(std::path::PathBuf::from);
        Self {
            epoch_start: Mutex::new(epoch_start_file.is_none().then(Instant::now)),
            epoch_start_file,
            ledger,
            generators,
            aec,
            reps,
            flooder: Mutex::new(flooder),
            incoming: Default::default(),
            recovery: Default::default(),
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
        let seconds = self.ledger.epoch_length.load(Ordering::Relaxed);
        start.is_some_and(|s| epoch_due(s, Instant::now(), epoch, seconds))
    }

    fn tick(&self) {
        if self.ledger.epoch_length.load(Ordering::Relaxed) == 0 {
            return;
        }
        let tick_started = Instant::now();
        let mut state = self.state.lock().unwrap();
        let mut recovery = self.recovery.lock().unwrap();
        let incoming = std::mem::take(&mut *self.incoming.lock().unwrap());
        let incoming_count = incoming.len();
        let report_transport = state.last_send.elapsed() >= RETRANSMIT
            && std::env::var_os("RAI_CLOSE_VALIDATION_DIAGNOSTICS").is_some();
        for (p, channel) in incoming {
            if p.kind == 8 {
                let epoch = state.epoch;
                let weights = state.weights.clone();
                state.cut.receive(p, epoch, &weights);
            } else if p.kind == 7 {
                if recovery.replies.len() < 128 {
                    if let Some(page) = state.recovery_page(&p) {
                        recovery.replies.push_back((channel, page));
                    }
                }
            } else {
                let id = p.candidate_id();
                let source =
                    p.kind <= 2 && p.valid_vote() && !state.weights.weight(&p.voter).is_zero();
                state.receive(p);
                if source && state.candidates.contains_key(&id) {
                    let sources = recovery.sources.entry(id).or_default();
                    if !sources.contains(&channel) && sources.len() < 16 {
                        sources.push(channel);
                    }
                }
            }
        }
        let mut keys = Vec::new();
        self.reps.lock().unwrap().rep_priv_keys(&mut keys);
        let mut cut_requests = Vec::new();
        if self.epoch_deadline_reached(state.epoch)
            && self.ledger.begin_epoch_drain() == Some(state.epoch)
        {
            let epoch = state.epoch;
            let weights = state.weights.clone();
            if !state.cut.started {
                let entries = self.generators.pause_epoch_report(epoch, &self.aec);
                state.cut.started = true;
                for key in &keys {
                    if weights.weight(&key.public_key()).is_zero() {
                        continue;
                    }
                    for packet in super::epoch_cut::EpochCut::packets(epoch, &entries, key) {
                        state.cut.receive(packet.clone(), epoch, &weights);
                        state.cut.local.push(packet);
                    }
                }
                eprintln!(
                    "EPOCH_CUT_PAUSED {}",
                    serde_json::json!({"pid":std::process::id(),"epoch":epoch,"pending":entries.len(),"voting_epoch":epoch+1})
                );
                // Publish the report immediately, independently of close-round timing.
                state.last_send = Instant::now() - RETRANSMIT;
            }
            if state.cut.finish(&weights) {
                let roots = state.cut.roots.clone().unwrap();
                eprintln!(
                    "EPOCH_CUT_RESUMED {}",
                    serde_json::json!({"pid":std::process::id(),"epoch":epoch,"roots":roots.len()})
                );
                self.generators.resume_epoch_cut(epoch, roots);
            }
            if let Some(roots) = &state.cut.roots {
                let pending = self.aec.cut_pending(epoch, roots);
                if pending.is_empty() && !state.ready {
                    state.ready = true;
                    let round = state.round;
                    state.enter_round(round);
                    eprintln!(
                        "EPOCH_DRAINED {}",
                        serde_json::json!({"pid":std::process::id(),"epoch":epoch,"unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()})
                    );
                } else if state.last_send.elapsed() >= RETRANSMIT {
                    let targets = state.cut.recovery_targets(&pending);
                    if !targets.is_empty() {
                        let count = targets.len().min(256);
                        for offset in 0..count {
                            cut_requests.push(
                                targets[(state.cut.recovery_cursor + offset) % targets.len()],
                            );
                        }
                        state.cut.recovery_cursor =
                            (state.cut.recovery_cursor + count) % targets.len();
                    }
                    eprintln!(
                        "EPOCH_CUT_WAIT {}",
                        serde_json::json!({"pid":std::process::id(),"epoch":epoch,"pending":pending.len()})
                    );
                }
            }
        }
        let (mut outgoing, closed) = state.drive(&self.ledger, &keys);
        // After the drain, membership only changes through certificates that exist
        // on peers but not here: late cross-notarizations of timed-out forks, and
        // roots outside the cut that never reached quorum locally. Solicit those
        // elections directly at the retransmission cadence instead of leaving them
        // to the bounded recovery rotation, so every replica reaches the common
        // membership quickly.
        let mut recovering = 0;
        if state.ready
            && closed.is_none()
            && state
                .last_timeout_solicitation
                .is_none_or(|last| last.elapsed() >= RETRANSMIT)
        {
            state.last_timeout_solicitation = Some(Instant::now());
            let now = Instant::now();
            let behind = state.members() < state.highest_members;
            // One request per solicitation keeps reply load bounded; the cursor
            // rotates through the remaining targets on later solicitations.
            let candidates = self.aec.membership_recovery_targets(state.epoch, behind);
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
            cut_requests.extend(targets);
        }
        if closed.is_none() {
            let targets = state.reconciliation_targets(&self.ledger);
            if !targets.is_empty() {
                eprintln!(
                    "EPOCH_CLOSE_RECONCILE {}",
                    serde_json::json!({
                        "pid":std::process::id(),"epoch":state.epoch,"round":state.round,"missing":targets.len(),
                        "examples":targets.iter().take(3).map(|(hash, _)| hash).collect::<Vec<_>>()
                    })
                );
                cut_requests.extend(targets);
            }
        }
        if state.ready && state.last_send.elapsed() >= RETRANSMIT {
            if std::env::var_os("RAI_CLOSE_VALIDATION_DIAGNOSTICS").is_some() {
                for (id, candidate) in &state.candidates {
                    if !state.valid(*id, &self.ledger) {
                        if let Some(hashes) = &candidate.hashes {
                            eprintln!(
                                "EPOCH_CLOSE_INVALID {}",
                                serde_json::json!({
                                    "pid":std::process::id(), "epoch":state.epoch,"candidate":id,
                                    "diagnostic":self.ledger.epoch_close_candidate_diagnostic(state.epoch, hashes)
                                })
                            );
                        }
                    }
                }
            }
            eprintln!(
                "EPOCH_CLOSE_PROGRESS {}",
                serde_json::json!({
                    "rep": keys.first().map(|k| k.public_key()), "epoch":state.epoch, "round":state.round,
                    "members":state.local_snapshots.back().map(|(_, hashes)| hashes.len()),
                    "state":state.local_snapshots.back().map(|(digest, _)| *digest),
                    "parent":state.parent,
                    "highest":state.highest_members,
                    "holding":state.rounds.get(&state.round).is_some_and(|r| r.certificate(BlockHash::ZERO, VoteKind::Timeout)) && !state.may_start_next_round(),
                    "recovering":recovering,
                    "candidates":state.candidates.iter().map(|(id,c)| serde_json::json!({"id":id,"round":c.header.round,"pages":c.pages.len(),"expected":c.header.pages,"complete":c.hashes.is_some(),"valid":state.valid(*id,&self.ledger)})).collect::<Vec<_>>(),
                    "votes":state.rounds.iter().map(|(r,v)| (*r,v.votes.len())).collect::<BTreeMap<_,_>>()
                })
            );
        }

        if let Some(hashes) = closed.filter(|_| state.ready) {
            if let Ok(discarded) = self.aec.close_epoch(&self.ledger, state.epoch, &hashes) {
                eprintln!(
                    "EPOCH_CLOSED {}",
                    serde_json::json!({"epoch":state.epoch,"hash":Ledger::epoch_state_hash(&hashes),"blocks":hashes.len(),"round":state.round,"discarded":discarded})
                );
                let epoch = state.epoch;
                let digest = Ledger::epoch_state_hash(&hashes);
                state.advance(hashes.clone(), self.ledger.rep_weights.read().clone());
                for key in &keys {
                    let mut receipt = state.template(0, BlockHash::ZERO, digest);
                    receipt.epoch = epoch;
                    receipt.kind = 6;
                    receipt.sign(key);
                    state.receive(receipt.clone());
                    state.local_receipts.push(receipt);
                }
            }
        }
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
                state.archive_snapshots.clear();
            }
        }
        // Only compact votes and receipts are flooded. Delta pages are requested
        // from one peer on a digest mismatch and never broadcast.
        if state.last_send.elapsed() >= RETRANSMIT {
            state.queue_retransmission();
            state.last_send = Instant::now();
        }
        outgoing.extend(state.retransmission_burst());
        let mut targeted = if let Some(key) = keys
            .iter()
            .find(|k| !state.weights.weight(&k.public_key()).is_zero())
        {
            recovery.requests(&state, key)
        } else {
            Vec::new()
        };
        for _ in 0..32 {
            if let Some(reply) = recovery.replies.pop_front() {
                targeted.push(reply);
            } else {
                break;
            }
        }
        let cut_epoch = state.epoch;
        drop(recovery);
        drop(state);
        let outgoing_count = outgoing.len();
        let processing_ms = tick_started.elapsed().as_millis();
        let mut flooder = self.flooder.lock().unwrap();
        for chunk in cut_requests.chunks(rsnano_messages::ConfirmReq::HASHES_MAX) {
            flooder.flood_prs_and_some_non_prs(
                &Message::ConfirmReq(
                    rsnano_messages::ConfirmReq::new(chunk.to_vec()).with_epoch(cut_epoch),
                ),
                TrafficType::VoteReply,
                1.0,
            );
        }
        for packet in outgoing {
            flooder.flood_prs_and_some_non_prs(
                &Message::EpochClose(packet),
                TrafficType::VoteReply,
                1.0,
            );
        }
        for (channel, packet) in targeted {
            flooder.try_send_channel_id(
                channel,
                &Message::EpochClose(packet),
                TrafficType::VoteReply,
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
    fn epoch_close_recovery_limit_does_not_drop_authenticated_decision_headers() {
        let mut replica = state();
        replica.round = 64;
        for round in 0..64 {
            let mut vote = replica.template(round, BlockHash::ZERO, 1.into());
            vote.sign(&PrivateKey::from(1));
            let id = vote.candidate_id();
            replica.receive(vote);
            replica.candidates.get_mut(&id).unwrap().hashes = Some(vec![1.into()]);
        }
        let mut next = replica.template(65, BlockHash::ZERO, 1.into());
        next.sign(&PrivateKey::from(2));
        let id = next.candidate_id();
        replica.receive(next);
        assert!(
            replica.candidates.contains_key(&id),
            "retained snapshots must not suppress the next authenticated decision header"
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
        let digest = Ledger::epoch_state_hash(&hashes);
        let mut p = s.template(0, BlockHash::ZERO, digest);
        p.sign(&PrivateKey::from(1));
        s.receive(p.clone());
        s.candidates.get_mut(&p.candidate_id()).unwrap().hashes = Some(hashes.clone());
        s.advance(hashes.clone(), s.weights.clone());
        let mut receipt = s.template(0, BlockHash::ZERO, digest);
        receipt.epoch = 0;
        receipt.kind = 6;
        receipt.sign(&PrivateKey::from(1));
        s.local_receipts.push(receipt.clone());
        s.advance(vec![1.into(), 2.into()], s.weights.clone());
        assert!(s.archive.iter().any(|v| v == &p));
        assert_eq!(s.snapshot(&digest), Some(&hashes));
        for i in 1..=6 {
            receipt.sign(&PrivateKey::from(i));
            s.receive(receipt.clone());
        }
        assert!(s.archive_acknowledged(0, digest));
    }

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
    fn epoch_close_split_snapshots_converge_through_parent_linked_rounds() {
        use rsnano_ledger::{
            LedgerBuilder, LedgerConstants, test_helpers::UnsavedBlockLatticeBuilder,
        };
        let path = std::env::temp_dir().join(format!("rai-close-rounds-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(1).unwrap();
            let mut lattice = UnsavedBlockLatticeBuilder::new();
            let a = lattice.genesis().send(100, 1);
            let b = lattice.genesis().send(101, 1);
            ledger.process_one(&a).unwrap();
            ledger.process_one(&b).unwrap();
            ledger.confirm(a.hash());
            let old = ledger.epoch_close_candidate(0);
            ledger.confirm(b.hash());
            let recent = ledger.epoch_close_candidate(0);
            let mut replicas: Vec<_> = (0..6)
                .map(|_| {
                    let mut s = state();
                    s.ready = true;
                    s
                })
                .collect();
            // Two valid snapshots each receive three FIRSTs: second look notarizes
            // both, so there is no final vote for either candidate in this round.
            let mut initial = Vec::new();
            for i in 0..6 {
                let hashes = if i < 3 { old.clone() } else { recent.clone() };
                let mut p =
                    replicas[i].template(0, BlockHash::ZERO, Ledger::epoch_state_hash(&hashes));
                let id = p.candidate_id();
                for replica in &mut replicas {
                    replica.candidates.entry(id).or_insert_with(|| Candidate {
                        validated: Cell::new(false),
                        header: p.clone(),
                        pages: Default::default(),
                        removed_pages: Default::default(),
                        recovery_base: None,
                        last_delta_page: None,
                        hashes: Some(hashes.clone()),
                    });
                }
                let key = PrivateKey::from(i as u64 + 1);
                let r = replicas[i].rounds.entry(0).or_default();
                let signer = r.signers.entry(key.public_key()).or_default();
                signer.first = Some(id);
                signer.notarized.insert(id);
                p.sign(&key);
                initial.push(p);
            }
            for s in &mut replicas {
                for p in &initial {
                    s.receive(p.clone());
                }
            }
            let mut decisions = vec![None; 6];
            for _ in 0..30 {
                let mut packets = Vec::new();
                for (i, s) in replicas.iter_mut().enumerate() {
                    let (out, decision) = s.drive(&ledger, &[PrivateKey::from(i as u64 + 1)]);
                    if decision.is_some() {
                        decisions[i] = decision;
                    }
                    packets.extend(out);
                    packets.extend(s.packets());
                }
                for s in &mut replicas {
                    for p in &packets {
                        s.receive(p.clone());
                    }
                }
                if decisions.iter().all(Option::is_some) {
                    break;
                }
            }
            assert!(decisions.iter().all(Option::is_some));
            assert!(decisions.iter().all(|d| d == &decisions[0]));
            assert!(replicas.iter().all(|s| s.round > 0));
        }
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn epoch_close_must_include_late_finalized_block() {
        use rsnano_ledger::{
            LedgerBuilder, LedgerConstants, test_helpers::UnsavedBlockLatticeBuilder,
        };
        let path = std::env::temp_dir().join(format!("rai-close-omission-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(1).unwrap();
            let mut lattice = UnsavedBlockLatticeBuilder::new();
            let a = lattice.genesis().send(100, 1);
            let b = lattice.genesis().send(101, 1);
            ledger.process_one(&a).unwrap();
            ledger.process_one(&b).unwrap();
            ledger.confirm(a.hash());
            // The drain sees B's notarization before its FINAL certificate or
            // asynchronous cementation. Snapshot membership must see it too.
            ledger.record_epoch_block(0, b.clone());
            let old = ledger.epoch_close_candidate(0);
            assert!(old.contains(&b.hash()));
            assert!(ledger.epoch_close_candidate_valid(0, &old));
            ledger.confirm(b.hash());
            let recent = ledger.epoch_close_candidate(0);
            let mut replicas: Vec<_> = (0..6)
                .map(|_| {
                    let mut s = state();
                    s.ready = true;
                    s
                })
                .collect();
            // Reproduce four snapshots before cementation and two afterward.
            // Both now include the already notarized block.
            let mut initial = Vec::new();
            for i in 0..6 {
                let hashes = if i < 4 { old.clone() } else { recent.clone() };
                let mut p =
                    replicas[i].template(0, BlockHash::ZERO, Ledger::epoch_state_hash(&hashes));
                let id = p.candidate_id();
                for replica in &mut replicas {
                    replica.candidates.entry(id).or_insert_with(|| Candidate {
                        validated: Cell::new(false),
                        header: p.clone(),
                        pages: Default::default(),
                        removed_pages: Default::default(),
                        recovery_base: None,
                        last_delta_page: None,
                        hashes: Some(hashes.clone()),
                    });
                }
                let key = PrivateKey::from(i as u64 + 1);
                let r = replicas[i].rounds.entry(0).or_default();
                let signer = r.signers.entry(key.public_key()).or_default();
                signer.first = Some(id);
                signer.notarized.insert(id);
                p.sign(&key);
                initial.push(p);
            }
            for s in &mut replicas {
                for p in &initial {
                    s.receive(p.clone());
                }
            }
            let mut decisions = vec![None; 6];
            for _ in 0..30 {
                let mut packets = Vec::new();
                for (i, s) in replicas.iter_mut().enumerate() {
                    let (out, decision) = s.drive(&ledger, &[PrivateKey::from(i as u64 + 1)]);
                    if decision.is_some() {
                        decisions[i] = decision;
                    }
                    packets.extend(out);
                    packets.extend(s.packets());
                }
                for s in &mut replicas {
                    for p in &packets {
                        s.receive(p.clone());
                    }
                }
                if decisions.iter().all(Option::is_some) {
                    break;
                }
            }
            assert!(decisions.iter().all(Option::is_some));
            assert!(decisions.iter().all(|d| d == &decisions[0]));
            assert!(
                decisions
                    .iter()
                    .all(|d| d.as_ref().unwrap().contains(&b.hash())),
                "certified close omitted a block already finalized in this epoch"
            );
        }
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn epoch_close_leaderless_snapshots_refresh_without_replacing_first() {
        use rsnano_ledger::{
            LedgerBuilder, LedgerConstants, test_helpers::UnsavedBlockLatticeBuilder,
        };
        let path =
            std::env::temp_dir().join(format!("rai-close-leaderless-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(1).unwrap();
            let mut lattice = UnsavedBlockLatticeBuilder::new();
            let mut replicas: Vec<_> = (0..6).map(|_| state()).collect();
            let mut firsts = Vec::new();
            // Each PR sees a different prefix of the monotonically growing set.
            // Every PR must propose independently, without a leader's message.
            for (i, s) in replicas.iter_mut().enumerate() {
                let key = PrivateKey::from(i as u64 + 1);
                assert!(s.drive(&ledger, &[key.clone()]).0.is_empty());
                let block = lattice.genesis().send(100 + i as u64, 1);
                ledger.process_one(&block).unwrap();
                ledger.confirm(block.hash());
                s.ready = true;
                let (out, decision) = s.drive(&ledger, &[key]);
                assert!(decision.is_none());
                assert_eq!(out.len(), 1);
                assert_eq!(out[0].kind, 0);
                firsts.push(out[0].candidate_id());
            }
            assert_eq!(firsts.iter().collect::<HashSet<_>>().len(), 6);
            let latest = ledger.epoch_close_candidate(0);
            let mut packets = Vec::new();
            for (i, s) in replicas.iter_mut().enumerate() {
                let key = PrivateKey::from(i as u64 + 1);
                let previous_snapshot = s.last_snapshot;
                let (out, _) = s.drive(&ledger, &[key.clone()]);
                assert!(out.is_empty(), "ledger growth must not replace FIRST");
                assert_eq!(
                    s.last_snapshot, previous_snapshot,
                    "an immediate tick must not rescan metadata"
                );
                s.last_snapshot = Some(Instant::now() - RETRANSMIT);
                let (out, _) = s.drive(&ledger, &[key.clone()]);
                assert!(out.is_empty(), "recovery refresh must not replace FIRST");
                assert_eq!(
                    s.rounds[&0].signers[&key.public_key()].first,
                    Some(firsts[i])
                );
                assert_eq!(
                    s.snapshot(&Ledger::epoch_state_hash(&latest)),
                    Some(&latest)
                );
                assert_eq!(
                    s.candidates.len(),
                    1,
                    "refreshing a recovery base must not create unsigned proposals"
                );
                packets.extend(s.packets());
            }
            for s in &mut replicas {
                for p in &packets {
                    s.receive(p.clone());
                }
            }
            // Six distinct FIRST values force a timeout certificate. All PRs
            // retry with the now-identical snapshot and close the same value.
            let mut decisions = vec![None; 6];
            for _ in 0..30 {
                let mut packets = Vec::new();
                for (i, s) in replicas.iter_mut().enumerate() {
                    let (out, decision) = s.drive(&ledger, &[PrivateKey::from(i as u64 + 1)]);
                    if decision.is_some() {
                        decisions[i] = decision;
                    }
                    packets.extend(out);
                    packets.extend(s.packets());
                }
                for s in &mut replicas {
                    for p in &packets {
                        s.receive(p.clone());
                    }
                }
                if decisions.iter().all(Option::is_some) {
                    break;
                }
            }
            assert!(decisions.iter().all(|d| d.as_ref() == Some(&latest)));
            assert!(replicas.iter().all(|s| s.round == 1));
        }
        std::fs::remove_dir_all(path).unwrap();
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
    fn epoch_close_delta_requires_shared_base_and_matches_immutable_target() {
        let base = vec![BlockHash::from(1), BlockHash::from(2)];
        let target = vec![BlockHash::from(1), BlockHash::from(3)];
        let base_hash = Ledger::epoch_state_hash(&base);
        let target_hash = Ledger::epoch_state_hash(&target);
        let mut source = state();
        source.local_snapshots.push_back((base_hash, base.clone()));
        source
            .local_snapshots
            .push_back((target_hash, target.clone()));
        let mut receiver = state();
        receiver
            .local_snapshots
            .push_back((base_hash, base.clone()));
        let mut proposal = source.template(0, BlockHash::ZERO, target_hash);
        proposal.sign(&PrivateKey::from(1));
        let id = proposal.candidate_id();
        receiver.receive(proposal.clone());
        assert!(receiver.candidates[&id].hashes.is_none());
        let mut request = proposal;
        request.kind = 7;
        request.hashes = vec![99.into()];
        request.sign(&PrivateKey::from(2));
        assert!(
            source.recovery_page(&request).is_none(),
            "no shared base must never fall back to a full list"
        );
        request.hashes = vec![base_hash];
        request.sign(&PrivateKey::from(2));
        let page = source.recovery_page(&request).unwrap();
        assert_eq!(page.base, base_hash);
        assert_eq!(page.hashes, vec![BlockHash::from(3)]);
        assert_eq!(page.removed, vec![BlockHash::from(2)]);
        let mut forged = page.clone();
        forged.hashes = vec![4.into()];
        receiver.receive(forged);
        assert!(receiver.candidates[&id].hashes.is_none());
        let mut full = page.clone();
        full.base = BlockHash::ZERO;
        full.hashes = target.clone();
        full.removed.clear();
        receiver.receive(full);
        assert!(
            receiver.candidates[&id].hashes.is_none(),
            "full lists are not a recovery mode"
        );
        receiver.receive(page);
        assert_eq!(receiver.candidates[&id].hashes.as_ref(), Some(&target));
        assert_eq!(
            receiver.snapshot(&base_hash),
            Some(&base),
            "recovery must not mutate its base"
        );
        assert!(
            receiver
                .packets()
                .iter()
                .all(|p| p.hashes.is_empty() && p.removed.is_empty() && p.base.is_zero())
        );
    }

    #[test]
    fn epoch_close_reconciliation_solicits_members_without_local_membership() {
        let ledger = Ledger::new_null();
        let mut s = state();
        let members = vec![BlockHash::from(9), BlockHash::from(10)];
        let mut proposal = s.template(0, BlockHash::ZERO, Ledger::epoch_state_hash(&members));
        proposal.sign(&PrivateKey::from(1));
        let id = proposal.candidate_id();
        s.receive(proposal);
        assert!(
            s.reconciliation_targets(&ledger).is_empty(),
            "nothing to reconcile before the snapshot is reconstructed"
        );
        s.candidates.get_mut(&id).unwrap().hashes = Some(members.clone());
        s.last_reconcile = None;
        let targets = s.reconciliation_targets(&ledger);
        assert_eq!(
            targets.iter().map(|(hash, _)| *hash).collect::<Vec<_>>(),
            members
        );
        assert!(targets.iter().all(|(_, root)| *root == Root::from(0u64)));
        s.last_reconcile = None;
        assert!(
            s.reconciliation_targets(&ledger).is_empty(),
            "solicitations are rate limited per member"
        );
        for last in s.solicited.values_mut() {
            *last -= SOLICIT_INTERVAL;
        }
        s.last_reconcile = None;
        assert_eq!(s.reconciliation_targets(&ledger).len(), 2);
    }

    #[test]
    fn epoch_close_digest_only_votes_bind_to_local_snapshot() {
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
            let digest = Ledger::epoch_state_hash(&snapshot);
            let mut replica = state();
            for i in 1..=6 {
                let mut vote = replica.template(0, BlockHash::ZERO, digest);
                vote.sign(&PrivateKey::from(i));
                replica.receive(vote);
            }
            let (out, decision) = replica.drive(&ledger, &[]);
            assert!(out.is_empty());
            assert_eq!(
                decision.as_ref(),
                Some(&snapshot),
                "matching digest needs no delta or full hash list"
            );
        }
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn epoch_close_delta_pages_survive_reordering_and_base_eviction() {
        let base: Vec<BlockHash> = (1..=600).map(Into::into).collect();
        let target: Vec<BlockHash> = (301..=900).map(Into::into).collect();
        let base_hash = Ledger::epoch_state_hash(&base);
        let target_hash = Ledger::epoch_state_hash(&target);
        let mut source = state();
        source.local_snapshots.push_back((base_hash, base.clone()));
        source
            .local_snapshots
            .push_back((target_hash, target.clone()));
        let mut receiver = state();
        receiver.local_snapshots.push_back((base_hash, base));
        let mut proposal = source.template(0, BlockHash::ZERO, target_hash);
        proposal.sign(&PrivateKey::from(1));
        let id = proposal.candidate_id();
        receiver.receive(proposal.clone());
        let mut request = proposal;
        request.kind = 7;
        request.hashes = vec![base_hash];
        request.page = 1;
        request.sign(&PrivateKey::from(2));
        let last = source.recovery_page(&request).unwrap();
        assert_eq!(last.pages, 2);
        receiver.receive(last.clone());
        assert!(receiver.candidates[&id].hashes.is_none());
        source.local_snapshots.clear();
        receiver.local_snapshots.clear();
        request.page = 0;
        request.sign(&PrivateKey::from(2));
        let first = source
            .recovery_page(&request)
            .expect("cached delta retains its immutable meaning");
        receiver.receive(last);
        receiver.receive(first);
        assert_eq!(receiver.candidates[&id].hashes.as_ref(), Some(&target));
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
    fn next_round_waits_until_membership_catches_up_with_the_highest_proposal() {
        let ledger = Ledger::new_null();
        let key = PrivateKey::from(6);
        let mut s = state();
        s.ready = true;
        s.last_snapshot = Some(Instant::now());
        s.local_snapshots
            .push_back((1.into(), vec![1.into(), 2.into(), 3.into()]));
        s.enter_round(0);
        s.rounds
            .entry(0)
            .or_default()
            .signers
            .entry(key.public_key())
            .or_default()
            .first = Some(BlockHash::from(1));
        for rep in 1..=5 {
            let mut p = s.template(0, BlockHash::ZERO, (10 + rep).into());
            p.members = 5;
            p.sign(&PrivateKey::from(rep));
            s.receive(p);
        }
        assert_eq!(s.highest_members, 5);
        for rep in 1..=4 {
            let mut timeout = s.template(0, BlockHash::ZERO, BlockHash::ZERO);
            timeout.kind = 4;
            timeout.sign(&PrivateKey::from(rep));
            s.receive(timeout);
        }
        assert!(s.rounds[&0].certificate(BlockHash::ZERO, VoteKind::Timeout));
        s.drive(&ledger, &[key.clone()]);
        assert_eq!(s.round, 0, "behind the highest proposal: keep recovering");
        s.local_snapshots.push_back((
            2.into(),
            vec![1.into(), 2.into(), 3.into(), 4.into(), 5.into()],
        ));
        s.drive(&ledger, &[key]);
        assert_eq!(s.round, 1, "caught up: start the next round");
    }

    #[test]
    fn replica_at_the_highest_count_joins_a_round_started_by_f_plus_one_or_after_the_timer() {
        let ledger = Ledger::new_null();
        let key = PrivateKey::from(6);
        let mut setup = || {
            let mut s = state();
            s.ready = true;
            s.last_snapshot = Some(Instant::now());
            s.local_snapshots
                .push_back((1.into(), vec![1.into(), 2.into(), 3.into()]));
            s.enter_round(0);
            s.rounds
                .entry(0)
                .or_default()
                .signers
                .entry(key.public_key())
                .or_default()
                .first = Some(BlockHash::from(1));
            for rep in 1..=5 {
                let mut p = s.template(0, BlockHash::ZERO, (10 + rep).into());
                p.sign(&PrivateKey::from(rep));
                s.receive(p);
            }
            for rep in 1..=4 {
                let mut timeout = s.template(0, BlockHash::ZERO, BlockHash::ZERO);
                timeout.kind = 4;
                timeout.sign(&PrivateKey::from(rep));
                s.receive(timeout);
            }
            s
        };
        let mut s = setup();
        s.drive(&ledger, &[key.clone()]);
        assert_eq!(
            s.round, 0,
            "already at the highest count: wait for starters"
        );
        let mut starter = s.template(1, BlockHash::ZERO, 20.into());
        starter.sign(&PrivateKey::from(1));
        s.receive(starter);
        s.drive(&ledger, &[key.clone()]);
        assert_eq!(s.round, 0, "one starter is not more than f");
        let mut starter = s.template(1, BlockHash::ZERO, 20.into());
        starter.sign(&PrivateKey::from(2));
        s.receive(starter);
        s.drive(&ledger, &[key.clone()]);
        assert_eq!(s.round, 1, "f + 1 started the round");
        let mut s = setup();
        s.rounds.get_mut(&0).unwrap().started = Instant::now() - round_timeout(0);
        s.drive(&ledger, &[key]);
        assert_eq!(s.round, 1, "the round timer keeps rounds live");
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
    fn sorted_difference_reports_added_and_removed_members() {
        let before: Vec<BlockHash> = [1u64, 3, 5].into_iter().map(Into::into).collect();
        let after: Vec<BlockHash> = [1u64, 4, 5, 6].into_iter().map(Into::into).collect();
        let (added, removed) = sorted_difference(&before, &after);
        assert_eq!(added, vec![BlockHash::from(4), BlockHash::from(6)]);
        assert_eq!(removed, vec![BlockHash::from(3)]);
    }
}
