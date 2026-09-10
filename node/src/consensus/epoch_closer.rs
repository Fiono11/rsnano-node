//! Priority Kudzu close rounds. Voting epochs and canonical ledger epochs are independent.
use crate::{
    consensus::{AecService, VoteGenerators, election::kudzu::KudzuVotes},
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};
use rsnano_ledger::{Ledger, RepWeights};
use rsnano_messages::{EpochClose, Message};
use rsnano_network::TrafficType;
use rsnano_types::{Amount, BlockHash, PrivateKey, PublicKey, Signature, Vote, VoteKind};
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
    fn receive(&mut self, packet: EpochClose, weights: &RepWeights, total: Amount) {
        let id = packet.candidate_id();
        let mut vote = Vote::null();
        vote.kind = kind(packet.kind);
        vote.epoch = packet.round;
        vote.voter = packet.voter;
        vote.hashes = vec![id];
        if self.tally.insert(Arc::new(vote), id).is_ok() {
            self.votes.insert((packet.voter, packet.kind, id), packet);
            self.tally.tally(weights, total);
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
    hashes: Option<Vec<BlockHash>>,
}
struct State {
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
}
impl State {
    fn new(epoch: u64, weights: RepWeights, archive: Vec<EpochClose>) -> Self {
        let total = Amount::raw(
            weights
                .values()
                .fold(0u128, |s, w| s.saturating_add(w.number())),
        );
        Self {
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
        }
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
        }
    }
    fn receive(&mut self, p: EpochClose) {
        if p.kind == 6 {
            if p.epoch <= self.epoch
                && p.epoch >= self.epoch.saturating_sub(1)
                && !self.weights.weight(&p.voter).is_zero()
                && p.valid_receipt()
            {
                self.receipts.insert((p.epoch, p.voter), p.state);
            }
            return;
        }
        // Future rounds are admitted only one step ahead. Retransmission repairs reordering.
        if p.epoch != self.epoch || p.round > self.round + 1 {
            return;
        }
        if p.kind <= 4 {
            if self.weights.weight(&p.voter).is_zero() || !p.valid_vote() {
                return;
            }
            self.rounds
                .entry(p.round)
                .or_default()
                .receive(p, &self.weights, self.total);
        } else if p.pages > 0 && p.page < p.pages && !p.hashes.is_empty() {
            let id = p.candidate_id();
            if !self.candidates.contains_key(&id) && self.candidates.len() >= 64 {
                return;
            }
            let entry = self.candidates.entry(id).or_insert_with(|| Candidate {
                validated: Cell::new(false),
                header: p.clone(),
                pages: Default::default(),
                hashes: None,
            });
            if entry.header.pages != p.pages || entry.hashes.is_some() {
                return;
            }
            entry.pages.entry(p.page).or_insert(p.hashes);
            if entry.pages.len() == p.pages as usize {
                let hashes: Vec<_> = entry.pages.values().flatten().copied().collect();
                if hashes.windows(2).all(|w| w[0] < w[1])
                    && Ledger::epoch_state_hash(&hashes) == p.state
                {
                    entry.hashes = Some(hashes);
                }
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
        let mut packets = Vec::new();
        for r in self.rounds.values() {
            packets.extend(r.votes.values().cloned());
        }
        for c in self.candidates.values() {
            if let Some(hashes) = &c.hashes {
                for (page, chunk) in hashes.chunks(EpochClose::PAGE_SIZE).enumerate() {
                    let mut p = c.header.clone();
                    p.kind = 5;
                    p.page = page as u16;
                    p.pages = hashes.len().div_ceil(EpochClose::PAGE_SIZE) as u16;
                    p.hashes = chunk.to_vec();
                    packets.push(p);
                }
            }
        }
        packets
    }
    fn drive(
        &mut self,
        ledger: &Ledger,
        keys: &[PrivateKey],
    ) -> (Vec<EpochClose>, Option<Vec<BlockHash>>) {
        let mut out = Vec::new();
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
        // Updated snapshots are advertised within the round, but FIRST remains
        // immutable. A subsequent round starts with the latest ledger snapshot.
        if keys
            .iter()
            .any(|key| !self.weights.weight(&key.public_key()).is_zero())
        {
            let hashes = ledger.epoch_close_candidate(self.epoch);
            if hashes.len() <= EpochClose::PAGE_SIZE * EpochClose::MAX_PAGES as usize {
                let proposal =
                    self.template(self.round, self.parent, Ledger::epoch_state_hash(&hashes));
                let id = proposal.candidate_id();
                self.candidates.entry(id).or_insert_with(|| Candidate {
                    validated: Cell::new(false),
                    header: proposal.clone(),
                    pages: Default::default(),
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
            self.round += 1;
            self.rounds.entry(self.round).or_default().started = Instant::now();
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
        if self.rounds[&self.round].certificate(BlockHash::ZERO, VoteKind::Timeout) {
            let expired = self.round;
            self.round += 1;
            self.rounds.entry(self.round).or_default().started = Instant::now();
            // Retain certified ancestors; discard unsuccessful snapshot payloads.
            self.candidates.retain(|_, c| c.header.round != expired);
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
            state: Mutex::new(state),
        }
    }
    pub fn receive(&self, message: EpochClose) {
        let mut q = self.incoming.lock().unwrap();
        if q.len() < 4096 {
            q.push_back(message);
        }
    }
    fn epoch_deadline_reached(&self, epoch: u64) -> bool {
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
        let mut state = self.state.lock().unwrap();
        let incoming = std::mem::take(&mut *self.incoming.lock().unwrap());
        for p in incoming {
            state.receive(p);
        }
        if self.epoch_deadline_reached(state.epoch)
            && self.ledger.begin_epoch_drain() == Some(state.epoch)
            && !state.ready
            && self.generators.draining_complete(state.epoch, &self.aec)
        {
            debug_trace(|| serde_json::json!({"type":"drained","epoch":state.epoch}));
            state.ready = true;
            let round = state.round;
            state.rounds.entry(round).or_default().started = Instant::now();
            self.ledger
                .voting_epoch
                .store(state.epoch + 1, Ordering::Release);
            tracing::info!(
                epoch = state.epoch,
                "Epoch drained; starting close election"
            );
        }
        let mut keys = Vec::new();
        self.reps.lock().unwrap().rep_priv_keys(&mut keys);
        let (mut outgoing, closed) = state.drive(&self.ledger, &keys);
        if state.ready && state.last_send.elapsed() >= RETRANSMIT {
            eprintln!(
                "EPOCH_CLOSE_PROGRESS {}",
                serde_json::json!({
                    "rep": keys.first().map(|k| k.public_key()), "epoch":state.epoch, "round":state.round,
                    "candidates":state.candidates.iter().map(|(id,c)| serde_json::json!({"id":id,"round":c.header.round,"pages":c.pages.len(),"expected":c.header.pages,"complete":c.hashes.is_some(),"valid":state.valid(*id,&self.ledger)})).collect::<Vec<_>>(),
                    "votes":state.rounds.iter().map(|(r,v)| (*r,v.votes.len())).collect::<BTreeMap<_,_>>()
                })
            );
        }

        if let Some(hashes) = closed {
            if let Ok(discarded) = self.aec.close_epoch(&self.ledger, state.epoch, &hashes) {
                let archive = state.packets();
                eprintln!(
                    "EPOCH_CLOSED {}",
                    serde_json::json!({"epoch":state.epoch,"hash":Ledger::epoch_state_hash(&hashes),"blocks":hashes.len(),"round":state.round,"discarded":discarded})
                );
                let epoch = state.epoch;
                let digest = Ledger::epoch_state_hash(&hashes);
                let mut receipts = std::mem::take(&mut state.receipts);
                receipts.retain(|(e, _), _| *e >= epoch);
                *state = State::new(epoch + 1, self.ledger.rep_weights.read().clone(), archive);
                state.receipts = receipts;
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
            if state.archive_acknowledged(receipt.epoch, receipt.state) && !state.archive.is_empty()
            {
                debug_trace(
                    || serde_json::json!({"type":"archive_acknowledged","epoch":receipt.epoch,"state":receipt.state,"packets":state.archive.len()}),
                );
                state.archive.clear();
            }
        }
        // Votes are sent immediately; full manifests are paced independently.
        if state.last_send.elapsed() >= RETRANSMIT {
            outgoing.extend(state.local_receipts.iter().cloned());
            outgoing.extend(state.archive.iter().cloned());
            outgoing.extend(state.packets());
            state.last_send = Instant::now();
        }
        drop(state);
        let mut flooder = self.flooder.lock().unwrap();
        for packet in outgoing {
            flooder.flood_prs_and_some_non_prs(
                &Message::EpochClose(packet),
                TrafficType::VoteReply,
                1.0,
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
                let (out, _) = s.drive(&ledger, &[key.clone()]);
                assert!(out.is_empty(), "ledger growth must not replace FIRST");
                assert_eq!(
                    s.rounds[&0].signers[&key.public_key()].first,
                    Some(firsts[i])
                );
                let latest_id = s
                    .template(0, BlockHash::ZERO, Ledger::epoch_state_hash(&latest))
                    .candidate_id();
                assert_eq!(s.candidates[&latest_id].hashes.as_ref(), Some(&latest));
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
    fn epoch_close_pages_must_match_immutable_digest() {
        let mut s = state();
        let hashes = vec![BlockHash::from(1), BlockHash::from(2)];
        let mut p = s.template(0, BlockHash::ZERO, Ledger::epoch_state_hash(&hashes));
        let id = p.candidate_id();
        p.kind = 5;
        p.pages = 2;
        p.hashes = vec![hashes[0]];
        s.receive(p.clone());
        assert!(s.candidates[&id].hashes.is_none());
        p.page = 1;
        p.hashes = vec![hashes[1]];
        s.receive(p);
        assert_eq!(s.candidates[&id].hashes.as_ref(), Some(&hashes));
    }
    #[test]
    fn epoch_close_rejects_forged_votes_and_unjustified_future_rounds() {
        let mut s = state();
        let mut p = s.template(0, BlockHash::ZERO, 1.into());
        p.sign(&PrivateKey::from(1));
        p.state = 2.into();
        s.receive(p);
        assert!(s.rounds.is_empty());
        let mut p = s.template(9, BlockHash::ZERO, 1.into());
        p.sign(&PrivateKey::from(1));
        s.receive(p);
        assert!(s.rounds.is_empty());
    }
}
