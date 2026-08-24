use std::{
    any::Any,
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc::Receiver},
    time::Duration,
};

use rsnano_ledger::{Ledger, RepWeightCache};
use rsnano_messages::{ClosePayload, ClosePayloadKind, CloseReport, CloseVote, Message};
use rsnano_network::{ChannelId, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Amount, Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, QualifiedRoot,
    UnixMillisTimestamp, VoteError, VoteType,
};

use super::election::VoteSummary;
use super::{AecService, AecTickerPlugin};
use crate::{
    representatives::{QuorumSnapshot, RepresentativeTracker},
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};

const CUT_DOMAIN: &[u8] = b"RAI/CloseCut";
const RECORD_DOMAIN: &[u8] = b"RAI/CloseRecord";

fn hash_root(mut builder: Blake2HashBuilder, root: &QualifiedRoot) -> Blake2HashBuilder {
    builder = builder
        .update(root.root.as_bytes())
        .update(root.previous.as_bytes())
        .update(root.epoch.to_be_bytes());
    builder
}

pub fn close_cut_hash(epoch: u64, roots: &[QualifiedRoot]) -> BlockHash {
    let mut builder = Blake2HashBuilder::default()
        .update(CUT_DOMAIN)
        .update(epoch.to_be_bytes());
    for root in roots {
        builder = hash_root(builder, root);
    }
    builder.build().into()
}

pub fn close_record_hash(
    epoch: u64,
    previous: BlockHash,
    finalized: &[(QualifiedRoot, BlockHash)],
) -> BlockHash {
    let mut builder = Blake2HashBuilder::default()
        .update(RECORD_DOMAIN)
        .update(epoch.to_be_bytes())
        .update(previous.as_bytes());
    for (root, hash) in finalized {
        builder = hash_root(builder, root).update(hash.as_bytes());
    }
    builder.build().into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseElectionKind {
    Cut,
    Record,
}

impl CloseElectionKind {
    fn wire(self) -> u8 {
        match self {
            Self::Cut => 0,
            Self::Record => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseElection {
    pub kind: CloseElectionKind,
    pub epoch: u64,
    pub round: u32,
    pub value: BlockHash,
    local_value: BlockHash,
    value_updated: bool,
    known_values: HashSet<BlockHash>,
    candidates: HashSet<BlockHash>,
    votes: HashMap<PublicKey, VoteSummary>,
    second_look: HashSet<BlockHash>,
    has_quorum: bool,
    timeout_predicate: bool,
    finalized: bool,
    timed_out: bool,
}

impl CloseElection {
    fn new(kind: CloseElectionKind, epoch: u64, round: u32, value: BlockHash) -> Self {
        Self {
            kind,
            epoch,
            round,
            value,
            local_value: value,
            value_updated: false,
            known_values: HashSet::from([value]),
            candidates: HashSet::from([value]),
            votes: HashMap::new(),
            second_look: HashSet::new(),
            has_quorum: false,
            timeout_predicate: false,
            finalized: false,
            timed_out: false,
        }
    }

    pub fn apply_vote(
        &mut self,
        voter: PublicKey,
        value: BlockHash,
        vote_type: VoteType,
        weight: Amount,
        quorum: &QuorumSnapshot,
        now: Timestamp,
    ) -> Result<(), VoteError> {
        let vote = self
            .votes
            .entry(voter)
            .or_insert_with(|| VoteSummary::new(voter, value, UnixMillisTimestamp::new(0), now));
        vote.apply_phase(
            vote_type,
            value,
            UnixMillisTimestamp::new(vote_type as u64 + 1),
            now,
        )?;
        vote.weight = weight;
        self.value_updated |= self.known_values.insert(value);
        self.update_outcome(quorum);
        Ok(())
    }

    fn update_outcome(&mut self, quorum: &QuorumSnapshot) {
        let mut first_tallies = HashMap::<BlockHash, Amount>::new();
        let mut tallies = HashMap::<BlockHash, Amount>::new();
        let mut final_tallies = HashMap::<BlockHash, Amount>::new();
        for vote in self.votes.values() {
            if let Some(hash) = vote.first {
                *first_tallies.entry(hash).or_default() += vote.weight;
            }
            for hash in &vote.notarized {
                *tallies.entry(*hash).or_default() += vote.weight;
            }
            if let Some(hash) = vote.final_vote {
                *final_tallies.entry(hash).or_default() += vote.weight;
            }
        }
        let timeout: Amount = self
            .votes
            .values()
            .filter(|vote| vote.timeout)
            .map(|vote| vote.weight)
            .sum();
        let certificate = quorum.total_weight - quorum.faulty_weight - quorum.slack_weight;
        let winner = tallies
            .iter()
            .max_by(|(a_hash, a_weight), (b_hash, b_weight)| {
                a_weight.cmp(b_weight).then_with(|| b_hash.cmp(a_hash))
            })
            .map(|(hash, weight)| (*hash, *weight));
        if let Some((winner, weight)) = winner {
            self.value = winner;
            self.has_quorum |= weight >= certificate;
        }
        self.second_look = first_tallies
            .iter()
            .filter_map(|(hash, weight)| {
                (*weight > quorum.faulty_weight + quorum.slack_weight).then_some(*hash)
            })
            .collect();
        let all_first: Amount = first_tallies.values().copied().sum();
        let max_first = first_tallies.values().copied().max().unwrap_or_default();
        self.timeout_predicate = all_first - max_first > quorum.faulty_weight + quorum.slack_weight;
        for candidate in self.candidates.iter().copied() {
            if first_tallies.get(&candidate).copied().unwrap_or_default()
                >= quorum.total_weight - quorum.slack_weight
                || final_tallies.get(&candidate).copied().unwrap_or_default() >= certificate
            {
                self.value = candidate;
                self.finalized = true;
                break;
            }
        }
        self.timed_out = !self.finalized && timeout >= certificate;
    }

    pub fn is_finalized(&self) -> bool {
        self.finalized
    }

    pub fn is_timed_out(&self) -> bool {
        self.timed_out
    }

    fn add_candidate(&mut self, value: BlockHash) -> bool {
        let changed = self.local_value != value;
        self.local_value = value;
        self.value_updated |= self.known_values.insert(value) || changed;
        self.candidates.insert(value) || changed
    }

    fn vote_targets(&self) -> Vec<(BlockHash, VoteType)> {
        let mut targets = Vec::new();
        for hash in &self.second_look {
            if self.candidates.contains(hash) {
                targets.push((*hash, VoteType::NonFinal));
            }
        }
        if self.timeout_predicate {
            targets.push((self.value, VoteType::Timeout));
        } else if self.has_quorum && self.candidates.contains(&self.value) {
            targets.push((self.value, VoteType::Final));
        }
        targets
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClosingPhase {
    CollectingReports,
    ElectingCut(CloseElection),
    DrainingCut,
    ElectingRecord(CloseElection),
}

#[derive(Clone, Debug)]
struct ClosingEpoch {
    epoch: u64,
    phase: ClosingPhase,
    reports: HashMap<PublicKey, CloseReport>,
    cut: Vec<QualifiedRoot>,
    cut_candidates: HashMap<BlockHash, Vec<QualifiedRoot>>,
    finalized: Vec<(QualifiedRoot, BlockHash)>,
}

/// Minimal, in-memory epoch-close state machine. Certificates are produced by the
/// existing election kernel; this type only applies their lifecycle effects.
pub struct CloseCoordinator {
    epoch_duration: Duration,
    next_close_at: Timestamp,
    open_epoch: u64,
    latest_closed_epoch: u64,
    previous_record: BlockHash,
    closing: Option<ClosingEpoch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CloseVoteCacheKey {
    kind: u8,
    epoch: u64,
    round: u32,
    value: BlockHash,
    vote_type: VoteType,
}

impl From<&CloseVote> for CloseVoteCacheKey {
    fn from(vote: &CloseVote) -> Self {
        Self {
            kind: vote.kind,
            epoch: vote.epoch,
            round: vote.round,
            value: vote.value,
            vote_type: vote.vote_type,
        }
    }
}

/// Inactive close-vote cache, mirroring the slot vote-cache contract: votes
/// received before an election exists are replayed when that exact election starts.
#[derive(Default)]
struct CloseVoteCache {
    entries: HashMap<CloseVoteCacheKey, HashMap<PublicKey, CloseVote>>,
}

impl CloseVoteCache {
    const MAX_ELECTIONS: usize = 64;
    const MAX_VOTERS: usize = 1024;

    fn insert(&mut self, vote: CloseVote) {
        let key = CloseVoteCacheKey::from(&vote);
        #[cfg(not(feature = "rai_protocol"))]
        if !self.entries.contains_key(&key) && self.entries.len() >= Self::MAX_ELECTIONS {
            return;
        }
        let voters = self.entries.entry(key).or_default();
        #[cfg(feature = "rai_protocol")]
        {
            voters.insert(vote.voter, vote);
        }
        #[cfg(not(feature = "rai_protocol"))]
        if voters.len() < Self::MAX_VOTERS || voters.contains_key(&vote.voter) {
            voters.insert(vote.voter, vote);
        }
    }

    fn take(&mut self, election: &CloseElection) -> Vec<CloseVote> {
        let keys: Vec<_> = self
            .entries
            .keys()
            .filter(|key| {
                key.kind == election.kind.wire()
                    && key.epoch == election.epoch
                    && key.round == election.round
            })
            .copied()
            .collect();
        keys.into_iter()
            .flat_map(|key| {
                self.entries
                    .remove(&key)
                    .into_iter()
                    .flat_map(HashMap::into_values)
            })
            .collect()
    }

    fn remove_obsolete(&mut self, epoch: u64, kind: CloseElectionKind, round: u32) {
        self.entries.retain(|key, _| {
            key.epoch > epoch
                || (key.epoch == epoch && (key.kind != kind.wire() || key.round >= round))
        });
    }
}

impl CloseCoordinator {
    pub fn new(now: Timestamp, epoch_duration: Duration) -> Self {
        Self {
            epoch_duration,
            next_close_at: now + epoch_duration,
            open_epoch: 1,
            latest_closed_epoch: 0,
            previous_record: BlockHash::ZERO,
            closing: None,
        }
    }

    pub fn open_epoch(&self) -> u64 {
        self.open_epoch
    }

    fn start_epoch_at(&mut self, now: Timestamp) {
        self.next_close_at = now + self.epoch_duration;
    }

    pub fn closing_epoch(&self) -> Option<u64> {
        self.closing.as_ref().map(|close| close.epoch)
    }

    pub fn latest_closed_epoch(&self) -> u64 {
        self.latest_closed_epoch
    }

    pub fn phase(&self) -> Option<&ClosingPhase> {
        self.closing.as_ref().map(|close| &close.phase)
    }

    fn active_election(&self) -> Option<CloseElection> {
        match self.phase()? {
            ClosingPhase::ElectingCut(election) | ClosingPhase::ElectingRecord(election) => {
                Some(election.clone())
            }
            _ => None,
        }
    }

    pub fn apply_vote(
        &mut self,
        vote: &CloseVote,
        weight: Amount,
        quorum: &QuorumSnapshot,
        now: Timestamp,
    ) -> Option<(CloseElectionKind, BlockHash)> {
        let close = self.closing.as_mut()?;
        let election = match &mut close.phase {
            ClosingPhase::ElectingCut(election) | ClosingPhase::ElectingRecord(election) => {
                election
            }
            _ => return None,
        };
        if vote.epoch != election.epoch
            || vote.round != election.round
            || vote.kind != election.kind.wire()
        {
            return None;
        }
        election
            .apply_vote(vote.voter, vote.value, vote.vote_type, weight, quorum, now)
            .ok()?;
        tracing::info!(
            epoch = election.epoch,
            round = election.round,
            kind = ?election.kind,
            voter = ?vote.voter,
            vote_type = ?vote.vote_type,
            weight = weight.number(),
            votes = election.votes.len(),
            total_weight = quorum.total_weight.number(),
            faulty_weight = quorum.faulty_weight.number(),
            slack_weight = quorum.slack_weight.number(),
            finalized = election.is_finalized(),
            "close vote applied"
        );
        election
            .is_finalized()
            .then_some((election.kind, election.value))
    }

    pub fn accepts_vote(&self, vote: &CloseVote) -> bool {
        let Some(close) = &self.closing else {
            return false;
        };
        let election = match &close.phase {
            ClosingPhase::ElectingCut(election) | ClosingPhase::ElectingRecord(election) => {
                election
            }
            _ => return false,
        };
        vote.epoch == election.epoch
            && vote.round == election.round
            && vote.kind == election.kind.wire()
    }

    pub fn tick(
        &mut self,
        now: Timestamp,
        pending: impl IntoIterator<Item = QualifiedRoot>,
        finalized: impl IntoIterator<Item = (QualifiedRoot, BlockHash)>,
        key: &PrivateKey,
    ) -> Option<CloseReport> {
        if self.closing.is_some() || now < self.next_close_at {
            return None;
        }
        let pending: Vec<_> = pending.into_iter().collect();
        let finalized: Vec<_> = finalized.into_iter().collect();
        if pending.is_empty() && finalized.is_empty() {
            self.next_close_at += self.epoch_duration;
            return None;
        }
        let epoch = self.open_epoch;
        self.open_epoch += 1;
        self.next_close_at += self.epoch_duration;
        let report = CloseReport::new_with_finalized(epoch, pending, finalized.clone(), key);
        self.closing = Some(ClosingEpoch {
            epoch,
            phase: ClosingPhase::CollectingReports,
            reports: HashMap::new(),
            cut: Vec::new(),
            cut_candidates: HashMap::new(),
            finalized,
        });
        Some(report)
    }

    pub fn add_report(
        &mut self,
        report: CloseReport,
        weight: impl Fn(&PublicKey) -> Amount,
        total_weight: Amount,
        faulty_weight: Amount,
    ) -> Option<CloseElection> {
        let close = self.closing.as_mut()?;
        if report.epoch != close.epoch
            || !report.validate()
            || !matches!(
                close.phase,
                ClosingPhase::CollectingReports | ClosingPhase::ElectingCut(_)
            )
        {
            return None;
        }
        close.finalized.extend(report.finalized.iter().cloned());
        close.finalized.sort();
        close.finalized.dedup();
        if close.reports.contains_key(&report.reporter) {
            return None;
        }
        close.reports.insert(report.reporter, report);
        let received: Amount = close.reports.keys().map(&weight).sum();
        if received < total_weight - faulty_weight {
            return None;
        }
        let mut visibility = HashMap::<QualifiedRoot, Amount>::new();
        for report in close.reports.values() {
            let weight = weight(&report.reporter);
            for root in &report.pending {
                *visibility.entry(root.clone()).or_default() += weight;
            }
        }
        let mut cut: Vec<_> = visibility
            .into_iter()
            .filter_map(|(root, weight)| (weight > faulty_weight).then_some(root))
            .collect();
        cut.sort();
        let value = close_cut_hash(close.epoch, &cut);
        close.cut_candidates.insert(value, cut);
        match &mut close.phase {
            ClosingPhase::CollectingReports => {
                let election = CloseElection::new(CloseElectionKind::Cut, close.epoch, 0, value);
                close.phase = ClosingPhase::ElectingCut(election.clone());
                Some(election)
            }
            ClosingPhase::ElectingCut(election) => election.add_candidate(value).then(|| {
                election.value = value;
                election.clone()
            }),
            _ => None,
        }
    }

    pub fn close_timed_out(&mut self) -> Option<CloseElection> {
        let close = self.closing.as_mut()?;
        let current = match &close.phase {
            ClosingPhase::ElectingCut(election) | ClosingPhase::ElectingRecord(election) => {
                election
            }
            _ => return None,
        };
        if !current.value_updated {
            return None;
        }
        let mut next = CloseElection::new(
            current.kind,
            current.epoch,
            current.round + 1,
            current.local_value,
        );
        next.known_values = current.known_values.clone();
        next.candidates = current.candidates.clone();
        close.phase = match next.kind {
            CloseElectionKind::Cut => ClosingPhase::ElectingCut(next.clone()),
            CloseElectionKind::Record => ClosingPhase::ElectingRecord(next.clone()),
        };
        Some(next)
    }

    /// Returns the active epoch slot elections excluded by the decided cut.
    pub fn cut_finalized(
        &mut self,
        value: BlockHash,
        active: impl IntoIterator<Item = QualifiedRoot>,
    ) -> Option<Vec<QualifiedRoot>> {
        let close = self.closing.as_mut()?;
        if !matches!(close.phase, ClosingPhase::ElectingCut(_)) {
            return None;
        }
        close.cut = close.cut_candidates.get(&value)?.clone();
        let excluded = active
            .into_iter()
            .filter(|root| root.epoch == close.epoch && close.cut.binary_search(root).is_err())
            .collect();
        close.phase = ClosingPhase::DrainingCut;
        Some(excluded)
    }

    pub fn slot_terminated(
        &mut self,
        root: QualifiedRoot,
        finalized: Option<BlockHash>,
    ) -> Option<CloseElection> {
        let close = self.closing.as_mut()?;
        if close.phase != ClosingPhase::DrainingCut || close.cut.binary_search(&root).is_err() {
            return None;
        }
        if let Some(hash) = finalized {
            close.finalized.push((root.clone(), hash));
        }
        close.cut.retain(|pending| pending != &root);
        if !close.cut.is_empty() {
            return None;
        }
        close.finalized.sort_by(|a, b| a.0.cmp(&b.0));
        close.finalized.dedup_by(|a, b| a.0 == b.0);
        let election = CloseElection::new(
            CloseElectionKind::Record,
            close.epoch,
            0,
            close_record_hash(close.epoch, self.previous_record, &close.finalized),
        );
        close.phase = ClosingPhase::ElectingRecord(election.clone());
        Some(election)
    }

    pub fn finish_empty_drain(&mut self) -> Option<CloseElection> {
        let close = self.closing.as_mut()?;
        if close.phase != ClosingPhase::DrainingCut || !close.cut.is_empty() {
            return None;
        }
        close.finalized.sort_by(|a, b| a.0.cmp(&b.0));
        close.finalized.dedup_by(|a, b| a.0 == b.0);
        let election = CloseElection::new(
            CloseElectionKind::Record,
            close.epoch,
            0,
            close_record_hash(close.epoch, self.previous_record, &close.finalized),
        );
        close.phase = ClosingPhase::ElectingRecord(election.clone());
        Some(election)
    }

    /// Incorporates newly learned finalization evidence into the active record
    /// election. Earlier record hashes remain candidates, so votes received before
    /// this replica caught up can be replayed once their evidence is known locally.
    pub fn refresh_record(
        &mut self,
        finalized: impl IntoIterator<Item = (QualifiedRoot, BlockHash)>,
    ) -> Option<CloseElection> {
        let close = self.closing.as_mut()?;
        let ClosingPhase::ElectingRecord(election) = &mut close.phase else {
            return None;
        };
        let old_finalized = close.finalized.clone();
        close.finalized.extend(finalized);
        close
            .finalized
            .sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        close.finalized.dedup_by(|a, b| a.0 == b.0);
        if close.finalized == old_finalized {
            return None;
        }
        let value = close_record_hash(close.epoch, self.previous_record, &close.finalized);
        if !election.add_candidate(value) {
            return None;
        }
        election.value = value;
        Some(election.clone())
    }

    fn draining_roots(&self) -> Vec<QualifiedRoot> {
        let Some(close) = &self.closing else {
            return Vec::new();
        };
        if close.phase != ClosingPhase::DrainingCut {
            return Vec::new();
        }
        close.cut.clone()
    }

    pub fn record_finalized(&mut self, value: BlockHash) -> bool {
        let Some(close) = self.closing.as_ref() else {
            return false;
        };
        let ClosingPhase::ElectingRecord(election) = &close.phase else {
            return false;
        };
        if election.value != value {
            return false;
        }
        self.latest_closed_epoch = close.epoch;
        self.previous_record = value;
        self.closing = None;
        true
    }

    fn current_record_payload(&self) -> Option<(BlockHash, Vec<(QualifiedRoot, BlockHash)>)> {
        let close = self.closing.as_ref()?;
        let ClosingPhase::ElectingRecord(election) = &close.phase else {
            return None;
        };
        Some((election.local_value, close.finalized.clone()))
    }

    fn admit_record_payload(
        &mut self,
        base: BlockHash,
        target: BlockHash,
        payload: Vec<(QualifiedRoot, BlockHash)>,
    ) -> Option<CloseElection> {
        let previous_record = self.previous_record;
        let close = self.closing.as_mut()?;
        let ClosingPhase::ElectingRecord(election) = &mut close.phase else {
            return None;
        };
        if election.local_value != base
            || payload.windows(2).any(|items| items[0] >= items[1])
            || !close
                .finalized
                .iter()
                .all(|entry| payload.binary_search(entry).is_ok())
            || close_record_hash(close.epoch, previous_record, &payload) != target
        {
            return None;
        }
        close.finalized = payload;
        election.add_candidate(target);
        election.value = target;
        Some(election.clone())
    }
}

/// Clock-driven single-node vertical slice used by nanospam. Report and vote
/// networking can be layered onto the same coordinator without changing its
/// state transitions.
pub struct CloseTransitionPlugin {
    coordinator: CloseCoordinator,
    clock: Arc<SteadyClock>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    rep_weights: Arc<RepWeightCache>,
    rep_tracker: Arc<RepresentativeTracker>,
    ledger: Arc<Ledger>,
    draining: HashMap<QualifiedRoot, BlockHash>,
    report_rx: Receiver<CloseReport>,
    vote_rx: Receiver<CloseVote>,
    payload_rx: Receiver<(ClosePayload, ChannelId)>,
    flooder: Mutex<MessageFlooder>,
    local_report: Option<CloseReport>,
    pending_cut: Option<CloseElection>,
    finalized_cut: Option<CloseElection>,
    vote_cache: CloseVoteCache,
    epoch_start_file: Option<PathBuf>,
    epoch_started: bool,
    local_representative: Option<PublicKey>,
    fixed_committee: Vec<PublicKey>,
    metrics_file: Option<PathBuf>,
    cut_started: Option<(u64, Timestamp)>,
    cut_duration: Option<Duration>,
    cut_round: Option<u32>,
    record_started: Option<(u64, Timestamp)>,
    retained_record_payloads: HashMap<BlockHash, Vec<(QualifiedRoot, BlockHash)>>,
    reconciliation_targets: HashSet<BlockHash>,
    reconciliation_attempts: HashMap<(BlockHash, BlockHash), HashSet<ChannelId>>,
}

impl CloseTransitionPlugin {
    pub fn new(
        epoch_duration: Duration,
        clock: Arc<SteadyClock>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        rep_weights: Arc<RepWeightCache>,
        rep_tracker: Arc<RepresentativeTracker>,
        ledger: Arc<Ledger>,
        report_rx: Receiver<CloseReport>,
        vote_rx: Receiver<CloseVote>,
        payload_rx: Receiver<(ClosePayload, ChannelId)>,
        flooder: MessageFlooder,
    ) -> Self {
        let start_delay = std::env::var("NANO_RAI_EPOCH_START_DELAY_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default();
        let epoch_start_file = std::env::var_os("NANO_RAI_EPOCH_START_FILE").map(PathBuf::from);
        let local_representative = std::env::var("NANO_RAI_LOCAL_REPRESENTATIVE")
            .ok()
            .and_then(|value| PublicKey::decode_hex(&value));
        let fixed_committee = std::env::var("NANO_RAI_FIXED_COMMITTEE")
            .ok()
            .map(|value| value.split(',').filter_map(PublicKey::decode_hex).collect())
            .unwrap_or_default();
        let metrics_file = std::env::var_os("NANO_RAI_CLOSE_METRICS_FILE").map(PathBuf::from);
        Self {
            coordinator: CloseCoordinator::new(
                clock.now() + Duration::from_secs(start_delay),
                epoch_duration,
            ),
            clock,
            wallet_reps,
            rep_weights,
            rep_tracker,
            ledger,
            draining: HashMap::new(),
            report_rx,
            vote_rx,
            payload_rx,
            flooder: Mutex::new(flooder),
            local_report: None,
            pending_cut: None,
            finalized_cut: None,
            vote_cache: CloseVoteCache::default(),
            epoch_started: epoch_start_file.is_none(),
            epoch_start_file,
            local_representative,
            fixed_committee,
            metrics_file,
            cut_started: None,
            cut_duration: None,
            cut_round: None,
            record_started: None,
            retained_record_payloads: HashMap::new(),
            reconciliation_targets: HashSet::new(),
            reconciliation_attempts: HashMap::new(),
        }
    }

    fn start_epoch_if_ready(&mut self, aec: &AecService, now: Timestamp) -> bool {
        if self.epoch_started {
            return true;
        }
        let Some(path) = &self.epoch_start_file else {
            unreachable!();
        };
        if !path.exists() {
            return false;
        }
        // Wallet funding happens before nanospam opens the measured protocol epoch.
        // Do not carry those setup finalizations into epoch 1's close record.
        aec.begin_epoch_one();
        self.coordinator.start_epoch_at(now);
        self.epoch_started = true;
        tracing::info!(epoch = 1, "epoch opened");
        true
    }

    fn local_key(&self) -> Option<PrivateKey> {
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        if let Some(representative) = self.local_representative {
            keys.into_iter()
                .find(|key| key.public_key() == representative)
        } else {
            keys.into_iter().next()
        }
    }

    fn quorum_snapshot(&self) -> QuorumSnapshot {
        let mut quorum = self.rep_tracker.quorum_snapshot();
        if !self.fixed_committee.is_empty() {
            quorum.total_weight = self
                .fixed_committee
                .iter()
                .map(|representative| self.rep_weights.weight(representative))
                .sum();
            let budget = Amount::raw(quorum.total_weight.number().saturating_sub(1) / 5);
            quorum.faulty_weight = budget;
            quorum.slack_weight = budget;
        }
        quorum
    }

    fn publish_vote(&mut self, election: &CloseElection, key: &PrivateKey) {
        self.publish_vote_for(election, election.local_value, VoteType::First, key);
    }

    fn publish_vote_for(
        &mut self,
        election: &CloseElection,
        value: BlockHash,
        vote_type: VoteType,
        key: &PrivateKey,
    ) {
        let vote = CloseVote::new(
            election.epoch,
            election.round,
            election.kind.wire(),
            value,
            vote_type,
            key,
        );
        self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
            &Message::CloseVote(vote.clone()),
            TrafficType::Generic,
            1.0,
        );
        self.apply_vote(vote);
    }

    fn replay_cached_votes(&mut self, election: &CloseElection) {
        for vote in self.vote_cache.take(election) {
            self.apply_vote(vote);
        }
    }

    fn apply_vote(&mut self, vote: CloseVote) {
        if vote.kind == CloseElectionKind::Record.wire()
            && self.coordinator.active_election().is_some_and(|e| {
                e.kind == CloseElectionKind::Record && !e.candidates.contains(&vote.value)
            })
        {
            self.reconciliation_targets.insert(vote.value);
        }
        let quorum = self.quorum_snapshot();
        let outcome = self.coordinator.apply_vote(
            &vote,
            self.rep_weights.weight(&vote.voter),
            &quorum,
            self.clock.now(),
        );
        if let Some((kind, value)) = outcome {
            match kind {
                CloseElectionKind::Cut => {
                    self.vote_cache
                        .remove_obsolete(vote.epoch, CloseElectionKind::Cut, u32::MAX);
                    let cut =
                        CloseElection::new(CloseElectionKind::Cut, vote.epoch, vote.round, value);
                    self.pending_cut = Some(cut.clone());
                    self.finalized_cut = Some(cut);
                }
                CloseElectionKind::Record => {
                    if self.coordinator.record_finalized(value) {
                        let record_duration = self
                            .record_started
                            .take()
                            .filter(|(epoch, _)| *epoch == vote.epoch)
                            .map(|(_, started)| started.elapsed(self.clock.now()))
                            .unwrap_or_default();
                        let cut_duration = self.cut_duration.take().unwrap_or_default();
                        if let Some(path) = &self.metrics_file {
                            use std::io::Write;
                            if let Ok(mut file) = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(path)
                            {
                                let _ = writeln!(
                                    file,
                                    "{} {} {} {} {}",
                                    vote.epoch,
                                    cut_duration.as_micros(),
                                    record_duration.as_micros(),
                                    self.cut_round.take().unwrap_or_default(),
                                    vote.round
                                );
                            }
                        }
                        self.vote_cache.remove_obsolete(
                            vote.epoch,
                            CloseElectionKind::Record,
                            u32::MAX,
                        );
                        tracing::warn!(epoch = vote.epoch, "epoch closed");
                    }
                }
            }
            return;
        }
        if self
            .coordinator
            .active_election()
            .is_some_and(|election| election.is_timed_out())
        {
            let Some(next) = self.coordinator.close_timed_out() else {
                return;
            };
            tracing::warn!(
                epoch = next.epoch,
                round = next.round,
                kind = ?next.kind,
                "close election round advanced"
            );
            if let Some(key) = self.local_key() {
                self.publish_vote(&next, &key);
                self.replay_cached_votes(&next);
            }
        }
    }

    fn apply_report(&mut self, _aec: &AecService, key: &PrivateKey, report: CloseReport) {
        if !report.validate() {
            return;
        }
        let finalized = report.finalized.clone();
        let quorum = self.quorum_snapshot();
        if let Some(cut) = self.coordinator.add_report(
            report,
            |reporter| self.rep_weights.weight(reporter),
            quorum.total_weight,
            quorum.faulty_weight,
        ) {
            tracing::warn!(epoch = cut.epoch, round = cut.round, value = ?cut.value, "close cut started");
            self.cut_started = Some((cut.epoch, self.clock.now()));
            self.publish_vote(&cut, key);
            self.replay_cached_votes(&cut);
        } else if let Some(record) = self.coordinator.refresh_record(finalized) {
            self.start_record(key, record);
        }
    }

    fn drain_reports(&mut self, aec: &AecService, key: &PrivateKey) {
        while let Ok(report) = self.report_rx.try_recv() {
            self.apply_report(aec, key, report);
        }
    }

    fn drain_votes(&mut self) {
        while let Ok(vote) = self.vote_rx.try_recv() {
            if self.coordinator.accepts_vote(&vote) {
                self.apply_vote(vote);
            } else if self
                .coordinator
                .closing_epoch()
                .is_none_or(|epoch| vote.epoch >= epoch)
            {
                self.vote_cache.insert(vote);
            }
        }
    }

    fn start_record_if_drained(&mut self, key: &PrivateKey) {
        if !self.draining.is_empty() {
            return;
        }
        let Some(record) = self.coordinator.finish_empty_drain() else {
            return;
        };
        if self
            .record_started
            .is_none_or(|(epoch, _)| epoch != record.epoch)
        {
            self.record_started = Some((record.epoch, self.clock.now()));
        }
        tracing::info!(
            epoch = record.epoch,
            round = record.round,
            "close record started"
        );
        self.retain_current_record_payload();
        self.publish_vote(&record, key);
        self.replay_cached_votes(&record);
    }

    fn start_record(&mut self, key: &PrivateKey, record: CloseElection) {
        if self
            .record_started
            .is_none_or(|(epoch, _)| epoch != record.epoch)
        {
            self.record_started = Some((record.epoch, self.clock.now()));
        }
        tracing::info!(
            epoch = record.epoch,
            round = record.round,
            "close record started"
        );
        self.retain_current_record_payload();
        self.publish_vote(&record, key);
        self.replay_cached_votes(&record);
    }

    fn refresh_record(&mut self, aec: &AecService, key: &PrivateKey) {
        let Some(epoch) = self.coordinator.closing_epoch() else {
            return;
        };
        let Some(record) = self
            .coordinator
            .refresh_record(aec.finalized_epoch_slots(epoch))
        else {
            return;
        };
        tracing::info!(
            epoch = record.epoch,
            round = record.round,
            value = ?record.value,
            "close record updated"
        );
        self.retain_current_record_payload();
        self.publish_vote(&record, key);
        self.replay_cached_votes(&record);
    }

    fn retain_current_record_payload(&mut self) {
        if let Some((hash, payload)) = self.coordinator.current_record_payload() {
            self.retained_record_payloads.insert(hash, payload);
        }
    }

    fn drain_payloads(&mut self, key: &PrivateKey) {
        while let Ok((message, channel_id)) = self.payload_rx.try_recv() {
            match message.kind {
                ClosePayloadKind::Request => {
                    let kind = if message.election_kind != CloseElectionKind::Record.wire() {
                        ClosePayloadKind::UnknownBase
                    } else if let (Some(base), Some(target)) = (
                        self.retained_record_payloads.get(&message.base),
                        self.retained_record_payloads.get(&message.target),
                    ) {
                        if base.iter().all(|entry| target.binary_search(entry).is_ok()) {
                            let additions: Vec<_> = target
                                .iter()
                                .filter(|entry| base.binary_search(entry).is_err())
                                .cloned()
                                .collect();
                            if additions.len() <= 256 {
                                ClosePayloadKind::Delta(additions)
                            } else {
                                ClosePayloadKind::DeltaTooLarge
                            }
                        } else {
                            ClosePayloadKind::UnknownBase
                        }
                    } else {
                        ClosePayloadKind::UnknownBase
                    };
                    let response = ClosePayload { kind, ..message };
                    tracing::info!(base=?response.base, target=?response.target, response=?response.kind, "close payload request handled");
                    self.flooder.lock().unwrap().try_send_channel_id(
                        channel_id,
                        &Message::ClosePayload(response),
                        TrafficType::Generic,
                    );
                }
                ClosePayloadKind::UnknownBase | ClosePayloadKind::DeltaTooLarge => {
                    tracing::info!(base=?message.base, target=?message.target, response=?message.kind, "close payload request rejected");
                    self.reconciliation_attempts
                        .entry((message.base, message.target))
                        .or_default()
                        .insert(channel_id);
                }
                ClosePayloadKind::Delta(additions) => {
                    let Some(base_payload) = self.retained_record_payloads.get(&message.base)
                    else {
                        continue;
                    };
                    if additions.len() > 256
                        || additions.windows(2).any(|items| items[0] >= items[1])
                        || additions
                            .iter()
                            .any(|entry| base_payload.binary_search(entry).is_ok())
                    {
                        continue;
                    }
                    let mut payload = base_payload.clone();
                    payload.extend(additions);
                    payload.sort();
                    if payload.windows(2).any(|items| items[0].0 == items[1].0) {
                        continue;
                    }
                    if let Some(record) = self.coordinator.admit_record_payload(
                        message.base,
                        message.target,
                        payload.clone(),
                    ) {
                        tracing::info!(base=?message.base, target=?message.target, "close record reconciled");
                        self.retained_record_payloads
                            .insert(message.target, payload);
                        self.reconciliation_targets.remove(&message.target);
                        self.publish_vote(&record, key);
                        self.replay_cached_votes(&record);
                    }
                }
            }
        }
    }

    fn request_reconciliation(&mut self) {
        let Some((base, _)) = self.coordinator.current_record_payload() else {
            return;
        };
        let peers = self.rep_tracker.peered_reps();
        for target in self.reconciliation_targets.clone() {
            if target == base {
                self.reconciliation_targets.remove(&target);
                continue;
            }
            let attempts = self
                .reconciliation_attempts
                .entry((base, target))
                .or_default();
            if let Some(peer) = peers
                .iter()
                .find(|peer| !attempts.contains(&peer.channel_id))
            {
                attempts.insert(peer.channel_id);
                let request = ClosePayload::request(
                    self.coordinator.closing_epoch().unwrap_or_default(),
                    CloseElectionKind::Record.wire(),
                    base,
                    target,
                );
                tracing::info!(?base, ?target, channel=?peer.channel_id, "requesting close record delta");
                self.flooder.lock().unwrap().try_send_channel_id(
                    peer.channel_id,
                    &Message::ClosePayload(request),
                    TrafficType::Generic,
                );
            }
        }
    }

    fn apply_cut(&mut self, aec: &AecService, key: &PrivateKey, cut: &CloseElection) {
        let active = aec.epoch_slots(cut.epoch);
        let roots = active.iter().map(|(root, _)| root.clone());
        let Some(excluded) = self.coordinator.cut_finalized(cut.value, roots) else {
            return;
        };
        self.cut_duration = self
            .cut_started
            .take()
            .filter(|(epoch, _)| *epoch == cut.epoch)
            .map(|(_, started)| started.elapsed(self.clock.now()));
        self.cut_round = Some(cut.round);
        for root in excluded {
            let hashes = aec
                .election_for_root(&root)
                .map(|election| {
                    election
                        .candidate_blocks()
                        .keys()
                        .copied()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            self.ledger.roll_back_batch(&hashes, usize::MAX);
            aec.exclude_by_cut(&root);
            aec.apply_rolled_back_outcome(&root);
            tracing::info!(epoch = cut.epoch, ?root, "slot excluded by finalized cut");
        }
        self.draining = active
            .into_iter()
            .filter(|(root, _)| aec.is_active_root(root))
            .collect();
        tracing::info!(
            epoch = cut.epoch,
            pending = self.draining.len(),
            "cut finalized"
        );

        // A slot can terminate after it was reported but before the cut certificate
        // arrives. Such a slot is no longer active, so no later AEC transition can
        // notify the close coordinator. Reconcile it from the finalized snapshot now.
        let finalized: HashMap<_, _> = aec.finalized_epoch_slots(cut.epoch).into_iter().collect();
        for root in self.coordinator.draining_roots() {
            if self.draining.contains_key(&root) {
                continue;
            }
            if let Some(record) = self
                .coordinator
                .slot_terminated(root.clone(), finalized.get(&root).copied())
            {
                self.start_record(key, record);
            }
        }
        self.start_record_if_drained(key);
    }
}

impl AecTickerPlugin for CloseTransitionPlugin {
    fn run(&mut self, aec: &AecService) {
        let now = self.clock.now();
        if !self.start_epoch_if_ready(aec, now) {
            return;
        }
        let Some(key) = self.local_key() else {
            return;
        };

        if let Some(report) = &self.local_report {
            self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                &Message::CloseReport(report.clone()),
                TrafficType::Generic,
                1.0,
            );
        }

        // A replica that has already finalized the cut must keep advertising its
        // final support until the record closes. Otherwise two fast replicas can
        // leave a slower replica without enough final votes to reconstruct the cut
        // certificate after they move into draining.
        if let Some(cut) = self.finalized_cut.clone() {
            self.publish_vote_for(&cut, cut.value, VoteType::Final, &key);
        }

        if self.coordinator.closing_epoch().is_none() {
            let epoch = self.coordinator.open_epoch();
            let pending = aec.epoch_slots(epoch);
            let finalized = aec.finalized_epoch_slots(epoch);
            let Some(report) = self.coordinator.tick(
                now,
                pending.iter().map(|(root, _)| root.clone()),
                finalized,
                &key,
            ) else {
                return;
            };
            debug_assert_eq!(aec.advance_epoch(), epoch + 1);
            self.local_report = Some(report.clone());
            self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                &Message::CloseReport(report.clone()),
                TrafficType::Generic,
                1.0,
            );
            tracing::info!(
                epoch,
                next_epoch = epoch + 1,
                pending = pending.len(),
                "epoch closing; next epoch opened"
            );
            self.apply_report(aec, &key, report);
            self.drain_reports(aec, &key);
            return;
        }

        self.drain_reports(aec, &key);
        self.refresh_record(aec, &key);
        self.drain_payloads(&key);
        // Close votes may be emitted before every PR has entered the matching
        // election. Keep advertising the current candidate until it obtains a
        // certificate, just as reports are advertised while being collected.
        if let Some(election) = self.coordinator.active_election() {
            self.publish_vote(&election, &key);
        }
        self.drain_votes();
        self.request_reconciliation();
        if let Some(election) = self.coordinator.active_election() {
            for (value, vote_type) in election.vote_targets() {
                self.publish_vote_for(&election, value, vote_type, &key);
            }
        }
        if let Some(cut) = self.pending_cut.take() {
            self.apply_cut(aec, &key, &cut);
        }

        if matches!(self.coordinator.phase(), Some(ClosingPhase::DrainingCut)) {
            let epoch = self.coordinator.closing_epoch().unwrap_or_default();
            let finalized: HashMap<_, _> = aec.finalized_epoch_slots(epoch).into_iter().collect();
            let terminated: Vec<_> = self
                .draining
                .iter()
                .filter_map(|(root, hash)| {
                    if let Some(hash) = finalized.get(root) {
                        Some((root.clone(), Some(*hash)))
                    } else if aec.was_recently_confirmed(hash) {
                        Some((root.clone(), Some(*hash)))
                    } else if !aec.is_active_root(root) {
                        Some((root.clone(), None))
                    } else {
                        None
                    }
                })
                .collect();
            for (root, finalized) in terminated {
                self.draining.remove(&root);
                if let Some(record) = self.coordinator.slot_terminated(root, finalized) {
                    self.start_record(&key, record);
                }
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::Root;

    fn root(value: u64, epoch: u64) -> QualifiedRoot {
        QualifiedRoot::new(Root::from(value), BlockHash::from(value + 100)).with_epoch(epoch)
    }

    #[test]
    fn synchronized_start_waits_a_full_epoch_duration() {
        let initial = Timestamp::new_test_instance();
        let mut close = CloseCoordinator::new(initial, Duration::from_secs(5));
        let synchronized_start = initial + Duration::from_secs(20);
        close.start_epoch_at(synchronized_start);
        let key = PrivateKey::from(1);

        assert!(
            close
                .tick(synchronized_start + Duration::from_secs(4), [], [], &key)
                .is_none()
        );
        assert_eq!(
            close
                .tick(
                    synchronized_start + Duration::from_secs(5),
                    [],
                    [(root(9, 1), BlockHash::from(9))],
                    &key,
                )
                .unwrap()
                .epoch,
            1
        );
    }

    #[test]
    fn empty_epoch_is_not_closed() {
        let now = Timestamp::new_test_instance();
        let duration = Duration::from_secs(5);
        let key = PrivateKey::from(1);
        let mut close = CloseCoordinator::new(now, duration);

        assert!(close.tick(now + duration, [], [], &key).is_none());
        assert_eq!(close.open_epoch(), 1);
        assert_eq!(close.latest_closed_epoch(), 0);
        assert_eq!(close.closing_epoch(), None);

        assert_eq!(
            close
                .tick(now + duration * 2, [root(1, 1)], [], &key)
                .unwrap()
                .epoch,
            1
        );
    }

    #[test]
    fn minimum_close_transition() {
        let now = Timestamp::new_test_instance();
        let duration = Duration::from_secs(10);
        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
        ];
        let weights = keys
            .iter()
            .map(|key| (key.public_key(), Amount::raw(1)))
            .collect::<HashMap<_, _>>();
        let included = root(1, 1);
        let excluded = root(2, 1);
        let already_final = root(3, 1);
        let mut close = CloseCoordinator::new(now, duration);

        assert!(close.tick(now, [], [], &keys[0]).is_none());
        let local_report = close
            .tick(
                now + duration,
                [included.clone(), excluded.clone()],
                [(already_final.clone(), BlockHash::from(203))],
                &keys[0],
            )
            .unwrap();
        assert_eq!(close.open_epoch(), 2);
        assert_eq!(close.closing_epoch(), Some(1));

        assert!(
            close
                .add_report(
                    local_report,
                    |reporter| weights.get(reporter).copied().unwrap_or_default(),
                    Amount::raw(3),
                    Amount::raw(1),
                )
                .is_none()
        );
        let cut = close
            .add_report(
                CloseReport::new(1, [included.clone()], &keys[1]),
                |reporter| weights.get(reporter).copied().unwrap_or_default(),
                Amount::raw(3),
                Amount::raw(1),
            )
            .unwrap();
        assert_eq!(cut.kind, CloseElectionKind::Cut);

        let excluded_roots = close
            .cut_finalized(cut.value, [included.clone(), excluded.clone()])
            .unwrap();
        assert_eq!(excluded_roots, vec![excluded]);

        let record = close
            .slot_terminated(included.clone(), Some(BlockHash::from(201)))
            .unwrap();
        assert_eq!(record.kind, CloseElectionKind::Record);
        assert!(close.record_finalized(record.value));
        assert_eq!(close.latest_closed_epoch(), 1);
        assert_eq!(close.open_epoch(), 2);
        assert_eq!(close.closing_epoch(), None);
    }

    #[test]
    fn record_candidate_updates_when_finalization_evidence_arrives() {
        let now = Timestamp::new_test_instance();
        let key = PrivateKey::from(1);
        let first = root(1, 1);
        let learned = root(2, 1);
        let first_hash = BlockHash::from(201);
        let learned_hash = BlockHash::from(202);
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));

        close
            .tick(
                now + Duration::from_secs(1),
                [],
                [(first.clone(), first_hash)],
                &key,
            )
            .unwrap();
        let cut = close
            .add_report(
                CloseReport::new(1, [], &key),
                |_| Amount::raw(1),
                Amount::raw(1),
                Amount::ZERO,
            )
            .unwrap();
        close.cut_finalized(cut.value, []).unwrap();
        let initial = close.finish_empty_drain().unwrap();

        let updated = close
            .refresh_record([(learned.clone(), learned_hash)])
            .unwrap();
        assert_ne!(updated.value, initial.value);

        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(1),
            faulty_weight: Amount::ZERO,
            slack_weight: Amount::ZERO,
            ..Default::default()
        };
        let old_vote = CloseVote::new(
            1,
            0,
            CloseElectionKind::Record.wire(),
            initial.value,
            VoteType::Final,
            &key,
        );
        assert!(close.accepts_vote(&old_vote));

        let updated_vote = CloseVote::new(
            1,
            0,
            CloseElectionKind::Record.wire(),
            updated.value,
            VoteType::Final,
            &key,
        );
        assert!(close.accepts_vote(&updated_vote));
        assert_eq!(
            close.apply_vote(&updated_vote, Amount::raw(1), &quorum, now),
            Some((CloseElectionKind::Record, updated.value))
        );
    }

    #[test]
    fn reconciled_record_payload_must_extend_the_current_base() {
        let now = Timestamp::new_test_instance();
        let key = PrivateKey::from(1);
        let first = (root(1, 1), BlockHash::from(101));
        let second = (root(2, 1), BlockHash::from(102));
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        let report = close
            .tick(now + Duration::from_secs(1), [], [first.clone()], &key)
            .unwrap();
        let cut = close
            .add_report(report, |_| Amount::raw(1), Amount::raw(1), Amount::ZERO)
            .unwrap();
        close.cut_finalized(cut.value, []).unwrap();
        let record = close.finish_empty_drain().unwrap();

        let payload = vec![first.clone(), second];
        let target = close_record_hash(1, BlockHash::ZERO, &payload);
        assert!(
            close
                .admit_record_payload(record.value, target, payload)
                .is_some()
        );

        let removal = vec![first];
        let removal_target = close_record_hash(1, BlockHash::ZERO, &removal);
        assert!(
            close
                .admit_record_payload(target, removal_target, removal)
                .is_none()
        );
    }

    #[test]
    fn timeout_certificate_does_not_advance_without_a_value_update() {
        let now = Timestamp::new_test_instance();
        let key = PrivateKey::from(1);
        let weights = HashMap::from([(key.public_key(), Amount::raw(1))]);
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        let report = close
            .tick(
                now + Duration::from_secs(1),
                [],
                [(
                    QualifiedRoot::new_test_instance().with_epoch(1),
                    BlockHash::from(9),
                )],
                &key,
            )
            .unwrap();
        close
            .add_report(
                report,
                |reporter| weights.get(reporter).copied().unwrap_or_default(),
                Amount::raw(1),
                Amount::ZERO,
            )
            .unwrap();
        assert!(close.close_timed_out().is_none());
    }

    #[test]
    fn one_remote_value_update_allows_one_round_advance() {
        let now = Timestamp::new_test_instance();
        let key = PrivateKey::from(1);
        let weights = HashMap::from([(key.public_key(), Amount::raw(1))]);
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        let report = close
            .tick(
                now + Duration::from_secs(1),
                [],
                [(
                    QualifiedRoot::new_test_instance().with_epoch(1),
                    BlockHash::from(9),
                )],
                &key,
            )
            .unwrap();
        close
            .add_report(
                report,
                |reporter| weights.get(reporter).copied().unwrap_or_default(),
                Amount::raw(1),
                Amount::ZERO,
            )
            .unwrap();
        let ClosingPhase::ElectingCut(election) = &mut close.closing.as_mut().unwrap().phase else {
            panic!("expected cut election");
        };
        election
            .apply_vote(
                PrivateKey::from(2).public_key(),
                BlockHash::from(10),
                VoteType::First,
                Amount::raw(1),
                &QuorumSnapshot {
                    total_weight: Amount::raw(2),
                    faulty_weight: Amount::ZERO,
                    slack_weight: Amount::ZERO,
                    ..Default::default()
                },
                now,
            )
            .unwrap();

        let next = close.close_timed_out().unwrap();
        assert_eq!(next.kind, CloseElectionKind::Cut);
        assert_eq!(next.round, 1);
        assert_eq!(next.local_value, close_cut_hash(1, &[]));
        assert!(next.known_values.contains(&BlockHash::from(10)));
        assert!(close.close_timed_out().is_none());
    }

    #[test]
    fn close_election_uses_rai_fast_and_timeout_thresholds() {
        let now = Timestamp::new_test_instance();
        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(101),
            faulty_weight: Amount::raw(20),
            slack_weight: Amount::raw(20),
            ..Default::default()
        };
        let mut fast = CloseElection::new(CloseElectionKind::Cut, 1, 0, BlockHash::from(1));
        fast.apply_vote(
            PrivateKey::from(1).public_key(),
            BlockHash::from(1),
            VoteType::First,
            Amount::raw(81),
            &quorum,
            now,
        )
        .unwrap();
        assert!(fast.is_finalized());

        let mut timeout = CloseElection::new(CloseElectionKind::Record, 1, 0, BlockHash::from(2));
        timeout
            .apply_vote(
                PrivateKey::from(2).public_key(),
                BlockHash::from(2),
                VoteType::Timeout,
                Amount::raw(61),
                &quorum,
                now,
            )
            .unwrap();
        assert!(timeout.is_timed_out());
    }

    #[test]
    fn vote_received_before_close_election_is_replayed_on_start() {
        let key = PrivateKey::from(1);
        let election = CloseElection::new(CloseElectionKind::Cut, 7, 0, BlockHash::from(9));
        let vote = CloseVote::new(7, 0, 0, election.value, VoteType::First, &key);
        let mut cache = CloseVoteCache::default();
        cache.insert(vote.clone());

        assert_eq!(cache.take(&election), vec![vote]);
        assert!(cache.take(&election).is_empty());
    }

    #[test]
    fn new_report_evidence_adds_a_cut_candidate() {
        let now = Timestamp::new_test_instance();
        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
        ];
        let weights = keys
            .iter()
            .map(|key| (key.public_key(), Amount::raw(1)))
            .collect::<HashMap<_, _>>();
        let root = root(44, 1);
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        close
            .tick(
                now + Duration::from_secs(1),
                [],
                [(
                    QualifiedRoot::new_test_instance().with_epoch(1),
                    BlockHash::from(9),
                )],
                &keys[0],
            )
            .unwrap();
        let weight = |reporter: &PublicKey| weights.get(reporter).copied().unwrap_or_default();
        assert!(
            close
                .add_report(
                    CloseReport::new(1, [root.clone()], &keys[0]),
                    weight,
                    Amount::raw(3),
                    Amount::raw(1)
                )
                .is_none()
        );
        let first = close
            .add_report(
                CloseReport::new(1, [], &keys[1]),
                weight,
                Amount::raw(3),
                Amount::raw(1),
            )
            .unwrap();
        let second = close
            .add_report(
                CloseReport::new(1, [root], &keys[2]),
                weight,
                Amount::raw(3),
                Amount::raw(1),
            )
            .unwrap();
        assert_ne!(first.value, second.value);
        assert!(second.candidates.contains(&first.value));
        assert!(second.candidates.contains(&second.value));
    }
}
