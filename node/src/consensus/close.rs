use std::{
    any::Any,
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc::Receiver},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rsnano_ledger::{AnySet, ConfirmedSet, Ledger, LedgerSet, RepWeightCache};
use rsnano_messages::{
    ClosePayload, ClosePayloadKind, CloseReport, CloseVote, ConfirmAck, Message, Publish,
};
use rsnano_network::{ChannelId, TrafficType};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Account, Amount, Blake2HashBuilder, BlockHash, BlockPriority, PrivateKey, PublicKey,
    QualifiedRoot, UnixMillisTimestamp, VoteError, VoteType,
};

use super::election::VoteSummary;
use super::{
    AecInsertRequest, AecService, AecTickerPlugin, LocalVoteHistory, vote_cache::VoteCache,
};
use crate::{
    representatives::{QuorumSnapshot, RepresentativeTracker},
    transport::MessageFlooder,
    wallets::WalletRepresentatives,
};

const CUT_DOMAIN: &[u8] = b"RAI/CloseCut";
const RECORD_DOMAIN: &[u8] = b"RAI/CloseRecord";
const REPORT_REQUEST_INTERVAL: Duration = Duration::from_millis(250);

fn cut_delta(base: &[BlockHash], target: &[BlockHash]) -> (Vec<BlockHash>, Vec<BlockHash>) {
    let base: HashSet<_> = base.iter().copied().collect();
    let target: HashSet<_> = target.iter().copied().collect();
    let mut additions: Vec<_> = target.difference(&base).copied().collect();
    let mut removals: Vec<_> = base.difference(&target).copied().collect();
    additions.sort();
    removals.sort();
    (additions, removals)
}

fn apply_cut_delta(
    base: &[BlockHash],
    additions: &[BlockHash],
    removals: &[BlockHash],
) -> Option<Vec<BlockHash>> {
    let mut result: HashSet<_> = base.iter().copied().collect();
    let mut mutations = HashSet::new();
    for hash in removals {
        if !mutations.insert(*hash) || !result.remove(hash) {
            return None;
        }
    }
    for hash in additions {
        if !mutations.insert(*hash) || !result.insert(*hash) {
            return None;
        }
    }
    let mut result: Vec<_> = result.into_iter().collect();
    result.sort();
    Some(result)
}

fn record_delta(
    base: &[(QualifiedRoot, BlockHash)],
    target: &[(QualifiedRoot, BlockHash)],
) -> (Vec<(QualifiedRoot, BlockHash)>, Vec<QualifiedRoot>) {
    let base: HashMap<_, _> = base.iter().cloned().collect();
    let target: HashMap<_, _> = target.iter().cloned().collect();
    let mut upserts: Vec<_> = target
        .iter()
        .filter(|(root, hash)| base.get(*root) != Some(*hash))
        .map(|(root, hash)| (root.clone(), *hash))
        .collect();
    let mut removals: Vec<_> = base
        .keys()
        .filter(|root| !target.contains_key(*root))
        .cloned()
        .collect();
    upserts.sort_by(|a, b| a.0.cmp(&b.0));
    removals.sort();
    (upserts, removals)
}

fn apply_record_delta(
    base: &[(QualifiedRoot, BlockHash)],
    upserts: &[(QualifiedRoot, BlockHash)],
    removals: &[QualifiedRoot],
) -> Option<Vec<(QualifiedRoot, BlockHash)>> {
    let mut result: HashMap<_, _> = base.iter().cloned().collect();
    let mut mutations = HashSet::new();
    for root in removals {
        if !mutations.insert(root.clone()) || result.remove(root).is_none() {
            return None;
        }
    }
    for (root, hash) in upserts {
        if !mutations.insert(root.clone()) || result.get(root) == Some(hash) {
            return None;
        }
        result.insert(root.clone(), *hash);
    }
    let mut result: Vec<_> = result.into_iter().collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Some(result)
}

fn hash_root(mut builder: Blake2HashBuilder, root: &QualifiedRoot) -> Blake2HashBuilder {
    builder = builder
        .update(root.root.as_bytes())
        .update(root.previous.as_bytes())
        .update(root.epoch.to_be_bytes());
    builder
}

pub fn close_cut_hash(epoch: u64, hashes: &[BlockHash]) -> BlockHash {
    let mut builder = Blake2HashBuilder::default()
        .update(CUT_DOMAIN)
        .update(epoch.to_be_bytes());
    for hash in hashes {
        builder = builder.update(hash.as_bytes());
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

fn vote_requires_record_payload(kind: CloseElectionKind, vote_type: VoteType) -> bool {
    kind == CloseElectionKind::Record && vote_type != VoteType::Timeout
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseElection {
    pub kind: CloseElectionKind,
    pub epoch: u64,
    pub round: u32,
    pub value: BlockHash,
    local_value: BlockHash,
    pending_value: Option<BlockHash>,
    value_updated: bool,
    known_values: HashSet<BlockHash>,
    candidates: HashSet<BlockHash>,
    votes: HashMap<PublicKey, VoteSummary>,
    second_look: HashSet<BlockHash>,
    has_quorum: bool,
    timeout_predicate: bool,
    finalized: bool,
    timed_out: bool,
    finalization_round: bool,
    final_evidence_round: Option<u32>,
}

impl CloseElection {
    fn new(kind: CloseElectionKind, epoch: u64, round: u32, value: BlockHash) -> Self {
        Self {
            kind,
            epoch,
            round,
            value,
            local_value: value,
            pending_value: None,
            value_updated: false,
            known_values: HashSet::from([value]),
            candidates: HashSet::from([value]),
            votes: HashMap::new(),
            second_look: HashSet::new(),
            has_quorum: false,
            timeout_predicate: false,
            finalized: false,
            timed_out: false,
            finalization_round: false,
            final_evidence_round: None,
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
        if self.finalization_round != (vote_type == VoteType::Final) {
            return Err(VoteError::Invalid);
        }
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

    fn is_notarized(&self) -> bool {
        self.has_quorum
    }

    fn add_candidate(&mut self, value: BlockHash) -> bool {
        let inserted = self.candidates.insert(value);
        self.value_updated |= self.known_values.insert(value) || inserted;
        if value != self.local_value {
            self.pending_value = Some(value);
        }
        inserted
    }

    fn vote_targets(&self) -> Vec<(BlockHash, VoteType)> {
        if self.finalization_round {
            return vec![(self.local_value, VoteType::Final)];
        }
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
    cut: Vec<BlockHash>,
    cut_candidates: HashMap<BlockHash, Vec<BlockHash>>,
    record_candidates: HashMap<BlockHash, Vec<(QualifiedRoot, BlockHash)>>,
    deferred_cut_value: Option<BlockHash>,
    finalized: Vec<(QualifiedRoot, BlockHash)>,
}

struct ClosedEpoch {
    epoch: u64,
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

/// Close reports can arrive before a lagging replica opens their epoch. Retain
/// them until that epoch closes so the replica can reconstruct the report
/// certificate and start the matching cut election.
#[derive(Default)]
struct CloseReportCache {
    entries: HashMap<u64, HashMap<PublicKey, CloseReport>>,
}

impl CloseReportCache {
    fn insert(&mut self, report: CloseReport) {
        self.entries
            .entry(report.epoch)
            .or_default()
            .insert(report.reporter, report);
    }

    fn reports(&self, epoch: u64) -> Vec<CloseReport> {
        self.entries
            .get(&epoch)
            .into_iter()
            .flat_map(|reports| reports.values().cloned())
            .collect()
    }

    fn reporters(&self, epoch: u64) -> HashSet<PublicKey> {
        self.entries
            .get(&epoch)
            .map(|reports| reports.keys().copied().collect())
            .unwrap_or_default()
    }

    fn remove_closed(&mut self, epoch: u64) {
        self.entries.retain(|cached_epoch, _| *cached_epoch > epoch);
    }
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
                    && election.finalization_round == (key.vote_type == VoteType::Final)
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

    fn future_final_certificate(
        &self,
        election: &CloseElection,
        weight: impl Fn(&PublicKey) -> Amount,
        threshold: Amount,
    ) -> Vec<CloseVote> {
        // Final votes are only combinable within one election round and for one
        // value.  In particular, never turn a collection of sparse votes from
        // several future rounds into a synthetic certificate.
        self.entries
            .iter()
            .filter(|(key, _)| {
                key.kind == election.kind.wire()
                    && key.epoch == election.epoch
                    && key.round >= election.round
                    && key.vote_type == VoteType::Final
            })
            .filter_map(|(_, votes)| {
                let total: Amount = votes.keys().map(&weight).sum();
                (total >= threshold).then(|| votes.values().cloned().collect::<Vec<_>>())
            })
            .max_by_key(|votes| votes.first().map(|vote| vote.round).unwrap_or_default())
            .unwrap_or_default()
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
            && election.finalization_round == (vote.vote_type == VoteType::Final)
    }

    /// A replica can lag behind peers which have already entered a finalization
    /// round. Their Final votes are evidence for the notarized value and must not
    /// be discarded merely because the local proposal round is older.
    fn apply_same_round_final_evidence(
        &mut self,
        vote: &CloseVote,
        weight: Amount,
        quorum: &QuorumSnapshot,
        now: Timestamp,
    ) -> bool {
        let Some(close) = self.closing.as_mut() else {
            return false;
        };
        let election = match &mut close.phase {
            ClosingPhase::ElectingCut(election) | ClosingPhase::ElectingRecord(election) => {
                election
            }
            _ => return false,
        };
        if vote.vote_type != VoteType::Final
            || vote.epoch != election.epoch
            || vote.kind != election.kind.wire()
            || vote.round < election.round
            || election.finalization_round
        {
            return false;
        }
        election.final_evidence_round = Some(
            election
                .final_evidence_round
                .map_or(vote.round, |round| round.max(vote.round)),
        );
        election
            .apply_vote(
                vote.voter,
                vote.value,
                VoteType::NonFinal,
                weight,
                quorum,
                now,
            )
            .is_ok()
    }

    fn accepts_same_round_final_evidence(&self, vote: &CloseVote) -> bool {
        let Some(election) = self.active_election() else {
            return false;
        };
        vote.vote_type == VoteType::Final
            && vote.epoch == election.epoch
            && vote.kind == election.kind.wire()
            && vote.round >= election.round
            && !election.finalization_round
    }

    /// A final vote for round r+1 also proves notarization support for the same
    /// value in proposal round r. This lets a lagging replica enter r+1 before
    /// replaying the original final vote there.
    fn apply_successor_final_evidence(
        &mut self,
        vote: &CloseVote,
        weight: Amount,
        quorum: &QuorumSnapshot,
        now: Timestamp,
    ) -> bool {
        let Some(close) = self.closing.as_mut() else {
            return false;
        };
        let election = match &mut close.phase {
            ClosingPhase::ElectingCut(election) | ClosingPhase::ElectingRecord(election) => {
                election
            }
            _ => return false,
        };
        if vote.vote_type != VoteType::Final
            || vote.epoch != election.epoch
            || vote.kind != election.kind.wire()
            || vote.round != election.round + 1
            || election.finalization_round
        {
            return false;
        }
        election
            .apply_vote(
                vote.voter,
                vote.value,
                VoteType::NonFinal,
                weight,
                quorum,
                now,
            )
            .is_ok()
    }

    fn accepts_successor_final_evidence(&self, vote: &CloseVote) -> bool {
        let Some(election) = self.active_election() else {
            return false;
        };
        vote.vote_type == VoteType::Final
            && vote.epoch == election.epoch
            && vote.kind == election.kind.wire()
            && vote.round == election.round + 1
            && !election.finalization_round
    }

    pub fn tick(
        &mut self,
        now: Timestamp,
        pending: impl IntoIterator<Item = BlockHash>,
        finalized: impl IntoIterator<Item = (QualifiedRoot, BlockHash)>,
        key: &PrivateKey,
    ) -> Option<CloseReport> {
        if self.closing.is_some() || now < self.next_close_at {
            return None;
        }
        let pending: Vec<_> = pending.into_iter().collect();
        let finalized: Vec<_> = finalized.into_iter().collect();
        let epoch = self.open_epoch;
        self.open_epoch += 1;
        self.next_close_at += self.epoch_duration;
        let report = CloseReport::new(epoch, pending, key);
        self.closing = Some(ClosingEpoch {
            epoch,
            phase: ClosingPhase::CollectingReports,
            reports: HashMap::new(),
            cut: Vec::new(),
            cut_candidates: HashMap::new(),
            record_candidates: HashMap::new(),
            deferred_cut_value: None,
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
        if report.epoch != close.epoch || !report.validate() {
            return None;
        }
        if close.reports.contains_key(&report.reporter) {
            return None;
        }
        close.reports.insert(report.reporter, report);
        let received: Amount = close.reports.keys().map(&weight).sum();
        if received < total_weight - faulty_weight {
            return None;
        }
        let mut visibility = HashMap::<BlockHash, Amount>::new();
        for report in close.reports.values() {
            let weight = weight(&report.reporter);
            for hash in &report.pending {
                *visibility.entry(*hash).or_default() += weight;
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
            ClosingPhase::ElectingCut(election) if election.round == 0 => {
                // Round 0's proposal is immutable once voting has started. A late
                // report may change the locally derived cut, but publishing a new
                // First vote in the same round makes the result depend on message
                // ordering at each replica. Carry the newer value into round 1 if
                // round 0 times out instead.
                close.deferred_cut_value = (value != election.local_value).then_some(value);
                None
            }
            ClosingPhase::ElectingCut(election) => {
                election.add_candidate(value).then(|| election.clone())
            }
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
        let pending_changes_value = current
            .pending_value
            .is_some_and(|pending| pending != current.local_value);
        if !current.value_updated && !pending_changes_value && close.deferred_cut_value.is_none() {
            return None;
        }
        // A late close report can defer a newly-derived *cut* commitment while
        // cut round zero is already voting.  That value must never leak into a
        // later record election: cut and record hashes have different payload
        // domains, and proposing a cut hash as a record makes the record payload
        // permanently unavailable.  Record rounds only advance to record
        // candidates learned by record reconciliation.
        let deferred = (current.kind == CloseElectionKind::Cut)
            .then(|| close.deferred_cut_value.take())
            .flatten();
        let local_value = deferred
            .or(current.pending_value)
            .unwrap_or(current.local_value);
        let mut next =
            CloseElection::new(current.kind, current.epoch, current.round + 1, local_value);
        next.known_values = current.known_values.clone();
        next.candidates = current.candidates.clone();
        next.known_values.insert(local_value);
        next.candidates.insert(local_value);
        close.phase = match next.kind {
            CloseElectionKind::Cut => ClosingPhase::ElectingCut(next.clone()),
            CloseElectionKind::Record => ClosingPhase::ElectingRecord(next.clone()),
        };
        Some(next)
    }

    /// A notarization terminates the proposal round but is not finality. Move the
    /// notarized value into a dedicated subsequent round whose only legal local
    /// action is a final vote for that value.
    pub fn close_notarized(&mut self) -> Option<CloseElection> {
        let close = self.closing.as_mut()?;
        let current = match &close.phase {
            ClosingPhase::ElectingCut(election) | ClosingPhase::ElectingRecord(election) => {
                election
            }
            _ => return None,
        };
        if current.finalization_round
            || current.is_finalized()
            || !current.is_notarized()
            || !current.candidates.contains(&current.value)
        {
            return None;
        }
        let next_round = current.final_evidence_round.unwrap_or(current.round + 1);
        let mut next = CloseElection::new(current.kind, current.epoch, next_round, current.value);
        next.finalization_round = true;
        next.known_values = current.known_values.clone();
        next.candidates = current.candidates.clone();
        close.phase = match next.kind {
            CloseElectionKind::Cut => ClosingPhase::ElectingCut(next.clone()),
            CloseElectionKind::Record => ClosingPhase::ElectingRecord(next.clone()),
        };
        Some(next)
    }

    /// Returns active epoch slot elections excluded by the decided cut.
    pub fn cut_finalized(
        &mut self,
        value: BlockHash,
        active: impl IntoIterator<Item = (QualifiedRoot, BlockHash)>,
    ) -> Option<Vec<QualifiedRoot>> {
        let close = self.closing.as_mut()?;
        if !matches!(close.phase, ClosingPhase::ElectingCut(_)) {
            return None;
        }
        // No deferred cut proposal is meaningful after the certified cut has
        // selected its contents.  In particular it must not be consumed by the
        // subsequent record election.
        close.deferred_cut_value = None;
        close.cut = close.cut_candidates.get(&value)?.clone();
        let excluded = active
            .into_iter()
            .filter_map(|(root, hash)| {
                (root.epoch == close.epoch && close.cut.binary_search(&hash).is_err())
                    .then_some(root)
            })
            .collect();
        close.phase = ClosingPhase::DrainingCut;
        Some(excluded)
    }

    pub fn slot_terminated(
        &mut self,
        root: QualifiedRoot,
        hash: BlockHash,
        finalized: Option<BlockHash>,
    ) -> Option<CloseElection> {
        let close = self.closing.as_mut()?;
        if close.phase != ClosingPhase::DrainingCut || close.cut.binary_search(&hash).is_err() {
            return None;
        }
        if let Some(hash) = finalized {
            close.finalized.push((root.clone(), hash));
        }
        close.cut.retain(|pending| pending != &hash);
        if !close.cut.is_empty() {
            return None;
        }
        close.finalized.sort_by(|a, b| a.0.cmp(&b.0));
        close.finalized.dedup_by(|a, b| a.0 == b.0);
        let value = close_record_hash(close.epoch, self.previous_record, &close.finalized);
        close
            .record_candidates
            .insert(value, close.finalized.clone());
        let election = CloseElection::new(CloseElectionKind::Record, close.epoch, 0, value);
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
        let value = close_record_hash(close.epoch, self.previous_record, &close.finalized);
        close
            .record_candidates
            .insert(value, close.finalized.clone());
        let election = CloseElection::new(CloseElectionKind::Record, close.epoch, 0, value);
        close.phase = ClosingPhase::ElectingRecord(election.clone());
        Some(election)
    }

    /// Incorporates newly learned finalization evidence into the active record.
    pub fn refresh_record(
        &mut self,
        entries: impl IntoIterator<Item = (QualifiedRoot, BlockHash)>,
        quorum: &QuorumSnapshot,
    ) -> Option<CloseElection> {
        let close = self.closing.as_ref()?;
        let mut by_slot: HashMap<QualifiedRoot, BlockHash> =
            close.finalized.iter().cloned().collect();
        by_slot.extend(entries);
        self.rebuild_record(by_slot, quorum)
    }

    /// Replaces the record contents with a complete authoritative snapshot.
    fn rebuild_record(
        &mut self,
        entries: impl IntoIterator<Item = (QualifiedRoot, BlockHash)>,
        quorum: &QuorumSnapshot,
    ) -> Option<CloseElection> {
        let close = self.closing.as_mut()?;
        let ClosingPhase::ElectingRecord(election) = &mut close.phase else {
            return None;
        };
        let old_finalized = close.finalized.clone();
        let mut by_slot = HashMap::<QualifiedRoot, BlockHash>::new();
        for (root, hash) in entries {
            by_slot.insert(root, hash);
        }
        close.finalized = by_slot.into_iter().collect();
        close.finalized.sort_by(|a, b| a.0.cmp(&b.0));
        if close.finalized == old_finalized {
            return None;
        }
        let value = close_record_hash(close.epoch, self.previous_record, &close.finalized);
        close
            .record_candidates
            .insert(value, close.finalized.clone());
        if !election.add_candidate(value) {
            return None;
        }
        // Votes for an unknown commitment are retained, but cannot decide the
        // election until local evidence reconstructs and validates its payload.
        // Re-evaluate those stored votes as soon as the candidate becomes valid.
        election.update_outcome(quorum);
        Some(election.clone())
    }

    fn draining_hashes(&self) -> Vec<BlockHash> {
        let Some(close) = &self.closing else {
            return Vec::new();
        };
        if close.phase != ClosingPhase::DrainingCut {
            return Vec::new();
        }
        close.cut.clone()
    }

    fn record_finalized(&mut self, value: BlockHash) -> Option<ClosedEpoch> {
        let Some(close) = self.closing.as_ref() else {
            return None;
        };
        let ClosingPhase::ElectingRecord(election) = &close.phase else {
            return None;
        };
        if election.value != value {
            return None;
        }
        let result = ClosedEpoch {
            epoch: close.epoch,
            finalized: close.record_candidates.get(&value)?.clone(),
        };
        self.latest_closed_epoch = close.epoch;
        self.previous_record = value;
        self.closing = None;
        Some(result)
    }

    fn current_record_payload(&self) -> Option<(BlockHash, Vec<(QualifiedRoot, BlockHash)>)> {
        let close = self.closing.as_ref()?;
        let ClosingPhase::ElectingRecord(election) = &close.phase else {
            return None;
        };
        let base = election.pending_value.unwrap_or(election.local_value);
        Some((base, close.record_candidates.get(&base)?.clone()))
    }

    fn record_payloads(&self) -> Vec<(BlockHash, Vec<(QualifiedRoot, BlockHash)>)> {
        self.closing
            .as_ref()
            .map(|close| {
                close
                    .record_candidates
                    .iter()
                    .map(|(hash, payload)| (*hash, payload.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn cut_payloads(&self) -> Vec<(BlockHash, Vec<BlockHash>)> {
        self.closing
            .as_ref()
            .map(|close| {
                close
                    .cut_candidates
                    .iter()
                    .map(|(hash, payload)| (*hash, payload.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn cut_payload_hashes(&self) -> Vec<BlockHash> {
        let mut hashes: Vec<BlockHash> = self
            .closing
            .as_ref()
            .map(|close| close.cut_candidates.keys().copied().collect())
            .unwrap_or_default();
        hashes.sort();
        hashes
    }

    fn cut_payload(&self, value: BlockHash) -> Option<Vec<BlockHash>> {
        self.closing.as_ref()?.cut_candidates.get(&value).cloned()
    }

    fn admit_cut_payload(
        &mut self,
        base: BlockHash,
        target: BlockHash,
        payload: Vec<BlockHash>,
    ) -> Option<CloseElection> {
        let close = self.closing.as_mut()?;
        close.cut_candidates.get(&base)?;
        if payload.windows(2).any(|items| items[0] >= items[1])
            || close_cut_hash(close.epoch, &payload) != target
        {
            return None;
        }
        close.cut_candidates.insert(target, payload);
        if let ClosingPhase::ElectingCut(election) = &mut close.phase {
            election.add_candidate(target);
            Some(election.clone())
        } else {
            None
        }
    }

    fn admit_record_payload(
        &mut self,
        _base: BlockHash,
        target: BlockHash,
        payload: Vec<(QualifiedRoot, BlockHash)>,
        quorum: &QuorumSnapshot,
    ) -> Option<CloseElection> {
        let previous_record = self.previous_record;
        let close = self.closing.as_mut()?;
        let ClosingPhase::ElectingRecord(election) = &mut close.phase else {
            return None;
        };
        if payload.windows(2).any(|items| items[0] >= items[1])
            || payload.windows(2).any(|items| items[0].0 == items[1].0)
            || close_record_hash(close.epoch, previous_record, &payload) != target
        {
            return None;
        }

        // A peer's record can be an earlier (or simply less complete) view of
        // the epoch.  Learning it must never discard slot outcomes which this
        // replica has already validated.  Compatible records therefore form a
        // monotonic map keyed by slot; the canonical proposal is the hash of
        // their sorted union.
        let mut merged: HashMap<QualifiedRoot, BlockHash> =
            close.finalized.iter().cloned().collect();
        let mut compatible = true;
        for (root, hash) in &payload {
            if merged.get(root).is_some_and(|known| known != hash) {
                compatible = false;
                break;
            }
            merged.insert(root.clone(), *hash);
        }

        // Retain the advertised commitment as a candidate so already-received
        // votes for it can still be evaluated. A conflicting local view must not
        // make an otherwise certified target opaque: the target can be decided
        // directly, while the local slot-finalization lock prevents this replica
        // from signing an unsafe value. Compatible payloads additionally derive
        // a canonical union independent of delivery order.
        close.record_candidates.insert(target, payload);
        let inserted = election.candidates.insert(target);
        election.value_updated |= election.known_values.insert(target) || inserted;
        if compatible {
            let mut merged: Vec<_> = merged.into_iter().collect();
            merged.sort_by(|a, b| a.0.cmp(&b.0));
            let merged_target = close_record_hash(close.epoch, previous_record, &merged);
            close.finalized = merged.clone();
            close.record_candidates.insert(merged_target, merged);
            election.add_candidate(merged_target);
            if merged_target != election.local_value {
                election.pending_value = Some(merged_target);
                election.value_updated = true;
            }
        }
        election.update_outcome(quorum);
        Some(election.clone())
    }
}

#[derive(Default)]
struct RecordPayloadChunks {
    total: u16,
    chunks: Vec<Option<Vec<(QualifiedRoot, BlockHash)>>>,
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
    // A certified cut can contain multiple fork hashes for one root. Keep each
    // hash as independent work so none is overwritten before it is drained.
    draining: HashMap<BlockHash, QualifiedRoot>,
    decided_cut_slots: HashMap<BlockHash, QualifiedRoot>,
    report_rx: Receiver<CloseReport>,
    vote_rx: Receiver<CloseVote>,
    payload_rx: Receiver<(ClosePayload, ChannelId)>,
    flooder: Mutex<MessageFlooder>,
    local_report: Option<CloseReport>,
    local_reports: HashMap<u64, CloseReport>,
    last_report_request: HashMap<u64, Timestamp>,
    pending_cut: Option<CloseElection>,
    finalized_cut: Option<CloseElection>,
    finalized_record: Option<CloseElection>,
    finalized_cuts: HashMap<u64, CloseElection>,
    finalized_records: HashMap<u64, CloseElection>,
    report_cache: CloseReportCache,
    vote_cache: CloseVoteCache,
    epoch_start_file: Option<PathBuf>,
    close_ready_file: Option<PathBuf>,
    close_ready_written: bool,
    epoch_started: bool,
    epoch_one_baseline: Option<HashMap<Account, u64>>,
    local_representative: Option<PublicKey>,
    fixed_committee: Vec<PublicKey>,
    fixed_committee_ports: Vec<u16>,
    metrics_file: Option<PathBuf>,
    cut_started: Option<(u64, Timestamp)>,
    cut_duration: Option<Duration>,
    cut_round: Option<u32>,
    record_started: Option<(u64, Timestamp)>,
    retained_record_payloads: HashMap<(u64, BlockHash), Vec<(QualifiedRoot, BlockHash)>>,
    record_payload_chunks: HashMap<(u64, BlockHash), RecordPayloadChunks>,
    retained_cut_payloads: HashMap<(u64, BlockHash), Vec<BlockHash>>,
    reconciliation_targets: HashSet<(u64, BlockHash)>,
    cut_reconciliation_targets: HashSet<(u64, BlockHash)>,
    reconciliation_attempts: HashMap<(u64, BlockHash, BlockHash), HashSet<ChannelId>>,
    last_reconciliation_request: HashMap<(u64, BlockHash, BlockHash), Timestamp>,
    closed_epoch: Option<ClosedEpoch>,
    unresolved_cut: HashSet<BlockHash>,
    last_recovery_request: HashMap<BlockHash, Timestamp>,
    vote_history: Arc<LocalVoteHistory>,
    slot_vote_cache: Arc<VoteCache>,
    node_index: usize,
}

impl CloseTransitionPlugin {
    pub fn new(
        epoch_duration: Duration,
        clock: Arc<SteadyClock>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        rep_weights: Arc<RepWeightCache>,
        rep_tracker: Arc<RepresentativeTracker>,
        ledger: Arc<Ledger>,
        vote_history: Arc<LocalVoteHistory>,
        slot_vote_cache: Arc<VoteCache>,
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
        let close_ready_file = std::env::var_os("NANO_RAI_CLOSE_READY_FILE").map(PathBuf::from);
        let local_representative = std::env::var("NANO_RAI_LOCAL_REPRESENTATIVE")
            .ok()
            .and_then(|value| PublicKey::decode_hex(&value));
        let fixed_committee = std::env::var("NANO_RAI_FIXED_COMMITTEE")
            .ok()
            .map(|value| value.split(',').filter_map(PublicKey::decode_hex).collect())
            .unwrap_or_default();
        let fixed_committee_ports = std::env::var("NANO_RAI_FIXED_COMMITTEE_PEERING_PORTS")
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .filter_map(|port| port.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        let metrics_file = std::env::var_os("NANO_RAI_CLOSE_METRICS_FILE").map(PathBuf::from);
        let node_index = std::env::var("NANO_RAI_NODE_INDEX")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default();
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
            decided_cut_slots: HashMap::new(),
            report_rx,
            vote_rx,
            payload_rx,
            flooder: Mutex::new(flooder),
            local_report: None,
            local_reports: HashMap::new(),
            last_report_request: HashMap::new(),
            pending_cut: None,
            finalized_cut: None,
            finalized_record: None,
            finalized_cuts: HashMap::new(),
            finalized_records: HashMap::new(),
            report_cache: CloseReportCache::default(),
            vote_cache: CloseVoteCache::default(),
            epoch_started: epoch_start_file.is_none(),
            epoch_start_file,
            close_ready_file,
            close_ready_written: false,
            epoch_one_baseline: None,
            local_representative,
            fixed_committee,
            fixed_committee_ports,
            metrics_file,
            cut_started: None,
            cut_duration: None,
            cut_round: None,
            record_started: None,
            retained_record_payloads: HashMap::new(),
            record_payload_chunks: HashMap::new(),
            retained_cut_payloads: HashMap::new(),
            reconciliation_targets: HashSet::new(),
            cut_reconciliation_targets: HashSet::new(),
            reconciliation_attempts: HashMap::new(),
            last_reconciliation_request: HashMap::new(),
            closed_epoch: None,
            unresolved_cut: HashSet::new(),
            last_recovery_request: HashMap::new(),
            vote_history,
            slot_vote_cache,
            node_index,
        }
    }

    fn start_epoch_if_ready(&mut self, aec: &AecService, now: Timestamp) -> bool {
        if self.epoch_started {
            return true;
        }
        let Some(path) = self.epoch_start_file.clone() else {
            unreachable!();
        };
        if !path.exists() {
            return false;
        }
        // The start file is published before its synchronized wall-clock
        // deadline. Capture the boundary immediately in that quiet window;
        // waiting until a later tick observes the deadline lets early spam
        // blocks enter some replicas' baselines but not others'.
        if self.epoch_one_baseline.is_none() {
            self.epoch_one_baseline = Some(self.confirmed_height_snapshot());
        }
        let synchronized_start = std::fs::read_to_string(&path)
            .ok()
            .and_then(|value| value.parse::<u128>().ok())
            .and_then(|millis| u64::try_from(millis).ok())
            .map(Duration::from_millis)
            .and_then(|start| {
                let wall_now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
                if wall_now < start {
                    None
                } else {
                    Some(now - (wall_now - start))
                }
            });
        let Some(synchronized_start) = synchronized_start else {
            return false;
        };
        // Wallet funding happens before nanospam opens the measured protocol epoch.
        // Do not carry those setup finalizations into epoch 1's close record.
        aec.begin_epoch_one(self.epoch_one_baseline.take().unwrap_or_default());
        self.coordinator.start_epoch_at(synchronized_start);
        self.epoch_started = true;
        tracing::info!(epoch = 1, "epoch opened");
        true
    }

    fn confirmed_height_snapshot(&self) -> HashMap<Account, u64> {
        let any = self.ledger.any();
        let confirmed = self.ledger.confirmed();
        any.iter_accounts()
            .filter_map(|(account, _)| {
                confirmed
                    .get_conf_info(&account)
                    .map(|info| (account, info.height))
            })
            .collect()
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

    fn signal_close_ready(&mut self) {
        if self.close_ready_written {
            return;
        }
        if let Some(path) = &self.close_ready_file {
            if let Err(error) = std::fs::write(path, b"ready") {
                tracing::warn!(
                    node = self.node_index,
                    ?error,
                    "failed to signal close readiness"
                );
                return;
            }
        }
        self.close_ready_written = true;
        tracing::info!(node = self.node_index, "close participant ready");
    }

    fn classify_fixed_committee_channels(&self) {
        if self.fixed_committee.len() != self.fixed_committee_ports.len() {
            return;
        }
        for channel in self.flooder.lock().unwrap().channels() {
            let port = channel.peering_addr_or_peer_addr().port();
            if let Some(index) = self
                .fixed_committee_ports
                .iter()
                .position(|committee_port| *committee_port == port)
            {
                self.rep_tracker
                    .set_channel(self.fixed_committee[index], channel.channel_id());
            }
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
        let vote_type = if election.finalization_round {
            VoteType::Final
        } else {
            VoteType::First
        };
        self.publish_vote_for(election, election.local_value, vote_type, key);
    }

    fn publish_vote_for(
        &mut self,
        election: &CloseElection,
        value: BlockHash,
        vote_type: VoteType,
        key: &PrivateKey,
    ) {
        if vote_requires_record_payload(election.kind, vote_type) {
            let Some(payload) = self.retained_record_payloads.get(&(election.epoch, value)) else {
                tracing::warn!(
                    node = self.node_index,
                    epoch = election.epoch,
                    ?value,
                    "record vote suppressed: payload unavailable"
                );
                return;
            };
            // Proposal votes describe a replica's current view and may be
            // superseded by reconciliation. Only a Final vote makes those slot
            // values irreversible; locking on First would strand replicas on
            // different transient records and prevent a later quorum.
            if vote_type == VoteType::Final
                && !self
                    .vote_history
                    .try_lock_record_values(payload, key.public_key())
            {
                tracing::warn!(
                    node = self.node_index,
                    epoch = election.epoch,
                    ?value,
                    "record vote suppressed by conflicting slot finalization lock"
                );
                return;
            }
        }
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

    fn replay_future_final_certificate(&mut self, election: &CloseElection) {
        let quorum = self.quorum_snapshot();
        let threshold = quorum.total_weight - quorum.faulty_weight - quorum.slack_weight;
        let votes = self.vote_cache.future_final_certificate(
            election,
            |voter| self.rep_weights.weight(voter),
            threshold,
        );
        for vote in votes {
            self.apply_same_round_final_evidence(&vote);
        }
    }

    fn apply_vote(&mut self, vote: CloseVote) {
        if vote.kind == CloseElectionKind::Cut.wire()
            && self.coordinator.active_election().is_some_and(|e| {
                e.epoch == vote.epoch
                    && e.kind == CloseElectionKind::Cut
                    && !e.candidates.contains(&vote.value)
            })
        {
            self.cut_reconciliation_targets
                .insert((vote.epoch, vote.value));
        }
        if vote.kind == CloseElectionKind::Record.wire()
            && self.coordinator.active_election().is_some_and(|e| {
                e.epoch == vote.epoch
                    && e.kind == CloseElectionKind::Record
                    && !e.candidates.contains(&vote.value)
            })
        {
            self.reconciliation_targets.insert((vote.epoch, vote.value));
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
                    self.finalized_cut = Some(cut.clone());
                    self.retain_finalized_cut(cut);
                }
                CloseElectionKind::Record => {
                    if let Some(closed) = self.coordinator.record_finalized(value) {
                        let record = CloseElection::new(
                            CloseElectionKind::Record,
                            vote.epoch,
                            vote.round,
                            value,
                        );
                        self.finalized_record = Some(record.clone());
                        self.retain_finalized_record(record);
                        self.closed_epoch = Some(closed);
                        self.reconciliation_targets.clear();
                        self.cut_reconciliation_targets.clear();
                        self.reconciliation_attempts.clear();
                        self.last_reconciliation_request.clear();
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
                        self.report_cache.remove_closed(vote.epoch);
                        tracing::warn!(node = self.node_index, epoch = vote.epoch, "epoch closed");
                    }
                }
            }
            return;
        }
        if self.start_finalization_round() {
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

    fn start_finalization_round(&mut self) -> bool {
        if let Some(next) = self.coordinator.close_notarized() {
            tracing::warn!(
                epoch = next.epoch,
                round = next.round,
                kind = ?next.kind,
                value = ?next.value,
                "close election notarized; finalization round started"
            );
            if let Some(key) = self.local_key() {
                self.publish_vote_for(&next, next.local_value, VoteType::Final, &key);
                self.replay_cached_votes(&next);
            }
            true
        } else {
            false
        }
    }

    fn apply_successor_final_evidence(&mut self, vote: &CloseVote) {
        let quorum = self.quorum_snapshot();
        if self.coordinator.apply_successor_final_evidence(
            vote,
            self.rep_weights.weight(&vote.voter),
            &quorum,
            self.clock.now(),
        ) {
            self.start_finalization_round();
        }
    }

    fn apply_same_round_final_evidence(&mut self, vote: &CloseVote) {
        let quorum = self.quorum_snapshot();
        if self.coordinator.apply_same_round_final_evidence(
            vote,
            self.rep_weights.weight(&vote.voter),
            &quorum,
            self.clock.now(),
        ) {
            self.start_finalization_round();
        }
    }

    /// Completes a record whose already-stored votes became decisive when new
    /// slot evidence or a reconciled payload validated their commitment.
    fn finish_record_if_finalized(&mut self) {
        let Some(election) = self.coordinator.active_election().filter(|election| {
            election.kind == CloseElectionKind::Record && election.is_finalized()
        }) else {
            return;
        };
        let Some(closed) = self.coordinator.record_finalized(election.value) else {
            return;
        };
        let record = CloseElection::new(
            CloseElectionKind::Record,
            election.epoch,
            election.round,
            election.value,
        );
        self.finalized_record = Some(record.clone());
        self.retain_finalized_record(record);
        self.closed_epoch = Some(closed);
        self.reconciliation_targets.clear();
        self.cut_reconciliation_targets.clear();
        self.reconciliation_attempts.clear();
        self.last_reconciliation_request.clear();
        let record_duration = self
            .record_started
            .take()
            .filter(|(epoch, _)| *epoch == election.epoch)
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
                    election.epoch,
                    cut_duration.as_micros(),
                    record_duration.as_micros(),
                    self.cut_round.take().unwrap_or_default(),
                    election.round
                );
            }
        }
        self.vote_cache
            .remove_obsolete(election.epoch, CloseElectionKind::Record, u32::MAX);
        self.report_cache.remove_closed(election.epoch);
        tracing::warn!(
            node = self.node_index,
            epoch = election.epoch,
            "epoch closed"
        );
    }

    fn retain_finalized_cut(&mut self, election: CloseElection) {
        let oldest = election.epoch.saturating_sub(63);
        self.finalized_cuts.retain(|epoch, _| *epoch >= oldest);
        self.finalized_cuts.insert(election.epoch, election);
    }

    fn retain_finalized_record(&mut self, election: CloseElection) {
        let oldest = election.epoch.saturating_sub(63);
        self.finalized_records.retain(|epoch, _| *epoch >= oldest);
        self.finalized_records.insert(election.epoch, election);
    }

    fn apply_report(&mut self, aec: &AecService, key: &PrivateKey, report: CloseReport) {
        if !report.validate() {
            return;
        }
        if report.epoch <= self.coordinator.latest_closed_epoch() {
            return;
        }
        self.report_cache.insert(report.clone());
        if self.coordinator.closing_epoch() != Some(report.epoch) {
            return;
        }
        let report_epoch = report.epoch;
        tracing::info!(node = self.node_index, epoch = report.epoch, reporter = ?report.reporter, pending = report.pending.len(), "close report received");
        let finalized = aec.finalized_epoch_slots(report.epoch);
        let quorum = self.quorum_snapshot();
        let new_cut = self.coordinator.add_report(
            report,
            |reporter| self.rep_weights.weight(reporter),
            quorum.total_weight,
            quorum.faulty_weight,
        );
        for (hash, payload) in self.coordinator.cut_payloads() {
            self.retained_cut_payloads
                .insert((report_epoch, hash), payload);
        }
        if let Some(cut) = new_cut {
            tracing::warn!(node = self.node_index, epoch = cut.epoch, round = cut.round, value = ?cut.value, "close cut started");
            self.cut_started = Some((cut.epoch, self.clock.now()));
            self.publish_vote(&cut, key);
            self.replay_cached_votes(&cut);
        } else if let Some(record) = self.coordinator.refresh_record(finalized, &quorum) {
            self.start_record(aec, key, record);
            self.start_finalization_round();
            self.finish_record_if_finalized();
        }
    }

    fn drain_reports(&mut self, aec: &AecService, key: &PrivateKey) {
        while let Ok(report) = self.report_rx.try_recv() {
            self.apply_report(aec, key, report);
        }
        if let Some(epoch) = self.coordinator.closing_epoch() {
            for report in self.report_cache.reports(epoch) {
                self.apply_report(aec, key, report);
            }
        }
    }

    fn drain_votes(&mut self) {
        while let Ok(vote) = self.vote_rx.try_recv() {
            if self.coordinator.accepts_vote(&vote) {
                self.apply_vote(vote);
            } else if self.coordinator.accepts_same_round_final_evidence(&vote) {
                // Preserve future Final votes until one exact round/value has a
                // certificate. A lagging replica may be many rounds behind, but
                // votes from different rounds must never be combined.
                let election = self.coordinator.active_election().unwrap();
                if vote.kind == CloseElectionKind::Cut.wire()
                    && !election.candidates.contains(&vote.value)
                {
                    self.cut_reconciliation_targets
                        .insert((vote.epoch, vote.value));
                }
                if vote.kind == CloseElectionKind::Record.wire()
                    && !election.candidates.contains(&vote.value)
                {
                    self.reconciliation_targets.insert((vote.epoch, vote.value));
                }
                self.vote_cache.insert(vote.clone());
                self.replay_future_final_certificate(&election);
            } else if self
                .coordinator
                .closing_epoch()
                .is_none_or(|epoch| vote.epoch >= epoch)
            {
                self.vote_cache.insert(vote);
            }
        }
    }

    fn start_record_if_drained(&mut self, aec: &AecService, key: &PrivateKey) {
        if !self.draining.is_empty() || !self.unresolved_cut.is_empty() {
            return;
        }
        let Some(epoch) = self.coordinator.closing_epoch() else {
            return;
        };
        // A locally empty certified cut is not sufficient to start the record:
        // another replica may already have >f evidence for an epoch-qualified
        // alias which this replica is about to recreate. Wait for those
        // supported aliases, without blocking on locally excluded elections
        // which never obtained protocol-level support.
        let faulty_weight = self.quorum_snapshot().faulty_weight;
        if self
            .slot_vote_cache
            .supported_hashes_for_epoch(epoch, faulty_weight)
            .into_iter()
            .any(|hash| {
                self.ledger
                    .any()
                    .get_block(&hash)
                    .map(|block| block.qualified_root().with_epoch(epoch))
                    .is_none_or(|root| aec.epoch_slot_outcome(&root).is_none())
            })
        {
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
        tracing::warn!(
            node = self.node_index,
            epoch = record.epoch,
            round = record.round,
            "close record started"
        );
        self.suppress_non_cut_votes(aec, record.epoch);
        self.retain_current_record_payload();
        self.publish_vote(&record, key);
        self.replay_cached_votes(&record);
        self.replay_future_final_certificate(&record);
    }

    fn suppress_non_cut_votes(&self, aec: &AecService, epoch: u64) {
        aec.suppress_epoch_votes(epoch);
        let cut_roots: HashSet<_> = self.decided_cut_slots.values().cloned().collect();
        aec.resume_cut_votes(epoch, &cut_roots);
    }

    fn start_record(&mut self, aec: &AecService, key: &PrivateKey, record: CloseElection) {
        if self
            .record_started
            .is_none_or(|(epoch, _)| epoch != record.epoch)
        {
            self.record_started = Some((record.epoch, self.clock.now()));
        }
        tracing::warn!(
            node = self.node_index,
            epoch = record.epoch,
            round = record.round,
            "close record started"
        );
        self.suppress_non_cut_votes(aec, record.epoch);
        self.retain_current_record_payload();
        self.publish_vote(&record, key);
        self.replay_cached_votes(&record);
        self.replay_future_final_certificate(&record);
    }

    fn refresh_record(&mut self, aec: &AecService, key: &PrivateKey) {
        let Some(epoch) = self.coordinator.closing_epoch() else {
            return;
        };
        let cut_roots: HashSet<_> = self.decided_cut_slots.values().cloned().collect();
        // Epoch membership comes from signed vote certificates, not from the
        // replica's local wall-clock epoch assignment.
        let quorum = self.quorum_snapshot();
        // Vote-cache contents depend on local delivery and replay history.  By
        // this point >f support has already been used to recreate missing
        // epoch-specific elections; record membership must come from their
        // resolved AEC outcomes so every replica hashes the same state.
        let mut entries = aec.finalized_epoch_slots(epoch);
        entries.retain(|(root, _)| !cut_roots.contains(root));
        entries.extend(self.decided_cut_slots.iter().filter_map(|(_, root)| {
            aec.epoch_slot_outcome(root)
                .flatten()
                .map(|hash| (root.clone(), hash))
        }));
        let Some(record) = self.coordinator.refresh_record(entries, &quorum) else {
            return;
        };
        tracing::warn!(
            node = self.node_index,
            epoch = record.epoch,
            round = record.round,
            value = ?record.value,
            "close record updated"
        );
        self.retain_current_record_payload();
        self.publish_vote(&record, key);
        self.replay_cached_votes(&record);
        self.start_finalization_round();
        self.finish_record_if_finalized();
    }

    fn retain_current_record_payload(&mut self) {
        let epoch = self.coordinator.closing_epoch().unwrap_or_default();
        for (hash, payload) in self.coordinator.record_payloads() {
            self.retained_record_payloads.insert((epoch, hash), payload);
        }
        if let Some((hash, payload)) = self.coordinator.current_record_payload() {
            tracing::warn!(
                node = self.node_index,
                epoch = self.coordinator.closing_epoch().unwrap_or_default(),
                ?hash,
                entries = payload.len(),
                "close record payload"
            );
            self.retained_record_payloads.insert((epoch, hash), payload);
        }
    }

    fn drain_payloads(&mut self, key: &PrivateKey) {
        while let Ok((message, channel_id)) = self.payload_rx.try_recv() {
            match message.kind {
                ClosePayloadKind::Request => {
                    let kind = if message.election_kind == CloseElectionKind::Cut.wire() {
                        if let (Some(base), Some(target)) = (
                            self.retained_cut_payloads
                                .get(&(message.epoch, message.base)),
                            self.retained_cut_payloads
                                .get(&(message.epoch, message.target)),
                        ) {
                            let (additions, removals) = cut_delta(base, target);
                            ClosePayloadKind::CutDelta {
                                additions,
                                removals,
                            }
                        } else {
                            ClosePayloadKind::UnknownBase
                        }
                    } else if message.election_kind == CloseElectionKind::Record.wire() {
                        if let (Some(base), Some(target)) = (
                            self.retained_record_payloads
                                .get(&(message.epoch, message.base)),
                            self.retained_record_payloads
                                .get(&(message.epoch, message.target)),
                        ) {
                            let (upserts, removals) = record_delta(base, target);
                            if record_delta_fits_wire(&upserts, &removals) {
                                let response = ClosePayload {
                                    kind: ClosePayloadKind::RecordDelta { upserts, removals },
                                    ..message
                                };
                                tracing::info!(base=?response.base, target=?response.target, "close payload request handled");
                                self.flooder.lock().unwrap().try_send_channel_id(
                                    channel_id,
                                    &Message::ClosePayload(response),
                                    TrafficType::Generic,
                                );
                                continue;
                            }
                        }
                        if let Some(target) = self
                            .retained_record_payloads
                            .get(&(message.epoch, message.target))
                        {
                            const ENTRIES_PER_CHUNK: usize = 500;
                            let total = target.len().div_ceil(ENTRIES_PER_CHUNK) as u16;
                            for (index, entries) in target.chunks(ENTRIES_PER_CHUNK).enumerate() {
                                let response = ClosePayload {
                                    kind: ClosePayloadKind::RecordChunk {
                                        index: index as u16,
                                        total,
                                        entries: entries.to_vec(),
                                    },
                                    ..message.clone()
                                };
                                self.flooder.lock().unwrap().try_send_channel_id(
                                    channel_id,
                                    &Message::ClosePayload(response),
                                    TrafficType::Generic,
                                );
                            }
                            continue;
                        }
                        ClosePayloadKind::UnknownBase
                    } else {
                        ClosePayloadKind::UnknownBase
                    };
                    let response = ClosePayload { kind, ..message };
                    tracing::info!(base=?response.base, target=?response.target, "close payload request handled");
                    self.flooder.lock().unwrap().try_send_channel_id(
                        channel_id,
                        &Message::ClosePayload(response),
                        TrafficType::Generic,
                    );
                }
                ClosePayloadKind::UnknownBase => {
                    tracing::warn!(node=self.node_index, base=?message.base, target=?message.target, response=?message.kind, "close payload request rejected");
                    self.reconciliation_attempts
                        .entry((message.epoch, message.base, message.target))
                        .or_default()
                        .insert(channel_id);
                }
                ClosePayloadKind::RecordDelta { upserts, removals } => {
                    let Some(base) = self
                        .retained_record_payloads
                        .get(&(message.epoch, message.base))
                    else {
                        continue;
                    };
                    let Some(payload) = apply_record_delta(base, &upserts, &removals) else {
                        continue;
                    };
                    let quorum = self.quorum_snapshot();
                    if let Some(record) = self.coordinator.admit_record_payload(
                        message.base,
                        message.target,
                        payload.clone(),
                        &quorum,
                    ) {
                        tracing::warn!(node=self.node_index, base=?message.base, target=?message.target, "close record reconciled");
                        self.retained_record_payloads
                            .insert((message.epoch, message.target), payload);
                        // Admission can derive a new canonical union hash which
                        // was not present on the wire.  Retain every coordinator
                        // candidate before it can become the next-round local
                        // value.
                        self.retain_current_record_payload();
                        self.reconciliation_targets
                            .remove(&(message.epoch, message.target));
                        self.publish_vote(&record, key);
                        self.replay_cached_votes(&record);
                        self.start_finalization_round();
                        self.finish_record_if_finalized();
                    }
                }
                ClosePayloadKind::RecordChunk {
                    index,
                    total,
                    entries,
                } => {
                    if total == 0 || index >= total {
                        continue;
                    }
                    let chunks = self
                        .record_payload_chunks
                        .entry((message.epoch, message.target))
                        .or_default();
                    if chunks.total != total {
                        chunks.total = total;
                        chunks.chunks = vec![None; total as usize];
                    }
                    chunks.chunks[index as usize] = Some(entries);
                    if chunks.chunks.iter().any(Option::is_none) {
                        continue;
                    }
                    let payload: Vec<_> = chunks
                        .chunks
                        .iter_mut()
                        .flat_map(|chunk| chunk.take().unwrap())
                        .collect();
                    self.record_payload_chunks
                        .remove(&(message.epoch, message.target));
                    let quorum = self.quorum_snapshot();
                    if let Some(record) = self.coordinator.admit_record_payload(
                        message.base,
                        message.target,
                        payload.clone(),
                        &quorum,
                    ) {
                        self.retained_record_payloads
                            .insert((message.epoch, message.target), payload);
                        self.retain_current_record_payload();
                        self.reconciliation_targets
                            .remove(&(message.epoch, message.target));
                        self.publish_vote(&record, key);
                        self.replay_cached_votes(&record);
                        self.start_finalization_round();
                        self.finish_record_if_finalized();
                    }
                }
                ClosePayloadKind::CutDelta {
                    additions,
                    removals,
                } => {
                    if message.election_kind != CloseElectionKind::Cut.wire() {
                        continue;
                    }
                    let Some(base) = self
                        .retained_cut_payloads
                        .get(&(message.epoch, message.base))
                    else {
                        continue;
                    };
                    let Some(payload) = apply_cut_delta(base, &additions, &removals) else {
                        continue;
                    };
                    if close_cut_hash(message.epoch, &payload) != message.target {
                        continue;
                    }
                    let candidate = self.coordinator.admit_cut_payload(
                        message.base,
                        message.target,
                        payload.clone(),
                    );
                    self.retained_cut_payloads
                        .insert((message.epoch, message.target), payload);
                    self.cut_reconciliation_targets
                        .remove(&(message.epoch, message.target));
                    if let Some(cut) = candidate {
                        self.replay_cached_votes(&cut);
                    }
                }
                ClosePayloadKind::SlotRequest => {
                    if let Some(block) = self.ledger.any().get_block(&message.target) {
                        let root = block.qualified_root();
                        self.flooder.lock().unwrap().try_send_channel_id(
                            channel_id,
                            &Message::Publish(Publish::new_forward(block.into())),
                            TrafficType::Generic,
                        );
                        // Vote history is an optimization, not the source of truth for
                        // finality. A PR can always recreate its final vote when the
                        // requested block is already confirmed in its ledger.
                        if self.ledger.any().confirmed().block_exists(&message.target) {
                            self.vote_history.get_or_create_final_vote(
                                &root.root,
                                message.epoch,
                                message.target,
                                key,
                            );
                        }
                        // Return every locally authored vote for this slot and epoch.
                        // RAI phases can require an earlier vote as well as the latest
                        // one, so answering with only a current vote is insufficient.
                        for vote in self
                            .vote_history
                            .all_votes_for_epoch(&root.root, message.epoch)
                        {
                            self.flooder.lock().unwrap().try_send_channel_id(
                                channel_id,
                                &Message::ConfirmAck(ConfirmAck::new_with_rebroadcasted_vote(
                                    (*vote).clone(),
                                )),
                                TrafficType::Generic,
                            );
                        }
                    }
                }
                ClosePayloadKind::ReportRequest => {
                    if let Some(report) = self.local_reports.get(&message.epoch) {
                        self.flooder.lock().unwrap().try_send_channel_id(
                            channel_id,
                            &Message::CloseReport(report.clone()),
                            TrafficType::Generic,
                        );
                    }
                }
            }
        }
    }

    fn request_missing_reports(&mut self) {
        let Some(epoch) = self.coordinator.closing_epoch() else {
            return;
        };
        if !matches!(
            self.coordinator.phase(),
            Some(ClosingPhase::CollectingReports)
        ) {
            return;
        }
        let reporters = self.report_cache.reporters(epoch);
        if self
            .fixed_committee
            .iter()
            .all(|rep| reporters.contains(rep))
        {
            return;
        }
        let now = self.clock.now();
        if self
            .last_report_request
            .get(&epoch)
            .is_some_and(|last| last.elapsed(now) < REPORT_REQUEST_INTERVAL)
        {
            return;
        }
        self.last_report_request.insert(epoch, now);
        let request = ClosePayload {
            epoch,
            election_kind: CloseElectionKind::Cut.wire(),
            base: BlockHash::ZERO,
            target: BlockHash::ZERO,
            kind: ClosePayloadKind::ReportRequest,
        };
        self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
            &Message::ClosePayload(request),
            TrafficType::Generic,
            1.0,
        );
    }

    fn request_reconciliation(&mut self) {
        let current_epoch = self.coordinator.closing_epoch().unwrap_or_default();
        let mut cut_bases = self.coordinator.cut_payload_hashes();
        if cut_bases.is_empty()
            && let Some(cut) = &self.finalized_cut
        {
            cut_bases.push(cut.value);
        }
        if !cut_bases.is_empty() {
            let now = self.clock.now();
            for (epoch, target) in self.cut_reconciliation_targets.clone() {
                if epoch != current_epoch {
                    self.cut_reconciliation_targets.remove(&(epoch, target));
                    continue;
                }
                if cut_bases.contains(&target) {
                    self.cut_reconciliation_targets.remove(&(epoch, target));
                    continue;
                }
                for base in cut_bases.iter().copied() {
                    let last = self
                        .last_reconciliation_request
                        .entry((epoch, base, target))
                        .or_insert(Timestamp::new(0));
                    if now >= *last + Duration::from_millis(250) {
                        *last = now;
                        let request = ClosePayload::request(
                            epoch,
                            CloseElectionKind::Cut.wire(),
                            base,
                            target,
                        );
                        self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                            &Message::ClosePayload(request),
                            TrafficType::Generic,
                            1.0,
                        );
                    }
                }
            }
        }
        let bases: Vec<_> = self
            .coordinator
            .record_payloads()
            .into_iter()
            .map(|(hash, _)| hash)
            .collect();
        if bases.is_empty() {
            return;
        }
        let peers = self.rep_tracker.peered_reps();
        for (epoch, target) in self.reconciliation_targets.clone() {
            if epoch != current_epoch {
                self.reconciliation_targets.remove(&(epoch, target));
                continue;
            }
            if bases.contains(&target) {
                self.reconciliation_targets.remove(&(epoch, target));
                continue;
            }
            for base in bases.iter().copied() {
                let attempts = self
                    .reconciliation_attempts
                    .entry((epoch, base, target))
                    .or_default();
                if let Some(peer) = peers
                    .iter()
                    .find(|peer| !attempts.contains(&peer.channel_id))
                {
                    attempts.insert(peer.channel_id);
                    let request = ClosePayload::request(
                        epoch,
                        CloseElectionKind::Record.wire(),
                        base,
                        target,
                    );
                    tracing::warn!(node=self.node_index, ?base, ?target, channel=?peer.channel_id, "requesting close record delta");
                    self.flooder.lock().unwrap().try_send_channel_id(
                        peer.channel_id,
                        &Message::ClosePayload(request),
                        TrafficType::Generic,
                    );
                } else {
                    // Representative discovery is advisory and can lag behind the
                    // close protocol. Keep reconciliation live even when no channel
                    // is currently classified as a PR (or every classified channel
                    // has already been tried).
                    let now = self.clock.now();
                    let last = self
                        .last_reconciliation_request
                        .entry((epoch, base, target))
                        .or_insert(Timestamp::new(0));
                    if now >= *last + Duration::from_millis(250) {
                        *last = now;
                        let request = ClosePayload::request(
                            epoch,
                            CloseElectionKind::Record.wire(),
                            base,
                            target,
                        );
                        tracing::warn!(
                            node = self.node_index,
                            ?base,
                            ?target,
                            "broadcasting close record delta request"
                        );
                        self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                            &Message::ClosePayload(request),
                            TrafficType::Generic,
                            1.0,
                        );
                    }
                }
            }
        }
    }

    fn apply_cut(&mut self, aec: &AecService, key: &PrivateKey, cut: &CloseElection) {
        let active = aec.epoch_slots(cut.epoch);
        let Some(certified_hashes) = self.coordinator.cut_payload(cut.value) else {
            return;
        };
        let membership: Vec<_> = active
            .iter()
            .map(|(root, current)| {
                let certified = aec
                    .election_for_root(root)
                    .and_then(|election| {
                        election
                            .candidate_blocks()
                            .keys()
                            .find(|hash| certified_hashes.binary_search(hash).is_ok())
                            .copied()
                    })
                    .unwrap_or(*current);
                (root.clone(), certified)
            })
            .collect();
        let Some(excluded) = self.coordinator.cut_finalized(cut.value, membership) else {
            return;
        };
        self.cut_duration = self
            .cut_started
            .take()
            .filter(|(epoch, _)| *epoch == cut.epoch)
            .map(|(_, started)| started.elapsed(self.clock.now()));
        self.cut_round = Some(cut.round);
        for root in excluded {
            aec.exclude_by_cut(&root);
            tracing::info!(
                epoch = cut.epoch,
                ?root,
                "fresh slot votes suppressed by finalized cut"
            );
        }
        let mut active_by_hash = HashMap::new();
        for (root, _) in active {
            if let Some(election) = aec.election_for_root(&root) {
                for hash in election.candidate_blocks().keys() {
                    if certified_hashes.binary_search(hash).is_ok() {
                        active_by_hash.insert(*hash, root.clone());
                    }
                }
            }
        }
        let cut_hashes = self.coordinator.draining_hashes();
        self.decided_cut_slots = active_by_hash
            .iter()
            .filter_map(|(hash, root)| {
                cut_hashes
                    .binary_search(hash)
                    .is_ok()
                    .then_some((*hash, root.clone()))
            })
            .collect();
        let included: HashSet<_> = self.decided_cut_slots.values().cloned().collect();
        aec.resume_cut_votes(cut.epoch, &included);
        tracing::warn!(
            node = self.node_index,
            epoch = cut.epoch,
            cut_hash = ?cut.value,
            hashes = ?cut_hashes,
            local_slots = ?active_by_hash,
            "certified cut contents"
        );
        self.unresolved_cut = cut_hashes
            .iter()
            .filter(|hash| !active_by_hash.contains_key(hash))
            .copied()
            .collect();
        self.draining = known_cut_slots(self.coordinator.draining_hashes(), &active_by_hash);
        tracing::warn!(
            node = self.node_index,
            epoch = cut.epoch,
            pending = self.draining.len(),
            unresolved = self.unresolved_cut.len(),
            "cut finalized"
        );

        self.start_record_if_drained(aec, key);
    }

    fn recover_cut_data(&mut self, aec: &AecService) {
        let epoch = self.coordinator.closing_epoch().unwrap_or_default();
        for hash in self.unresolved_cut.clone() {
            if let Some(block) = self.ledger.any().get_block(&hash) {
                let root = block.qualified_root().with_epoch(epoch);
                if aec.has_election_for_epoch(&hash, epoch) {
                    tracing::warn!(
                        node = self.node_index,
                        epoch,
                        ?hash,
                        ?root,
                        "recovered existing certified-epoch cut slot"
                    );
                    self.draining.insert(hash, root.clone());
                    self.decided_cut_slots.insert(hash, root);
                    self.unresolved_cut.remove(&hash);
                    continue;
                }
                if self.ledger.any().confirmed().block_exists(&hash) {
                    tracing::warn!(
                        node = self.node_index,
                        epoch,
                        ?hash,
                        ?root,
                        "recovered confirmed cut slot"
                    );
                    self.draining.insert(hash, root.clone());
                    self.decided_cut_slots.insert(hash, root);
                    self.unresolved_cut.remove(&hash);
                    continue;
                }
                let _ = aec.insert_for_epoch(
                    AecInsertRequest::new_manual(block, BlockPriority::new_test_instance()),
                    self.clock.now(),
                    epoch,
                );
                if aec.is_active_root(&root) || aec.epoch_slot_outcome(&root).is_some() {
                    tracing::warn!(
                        node = self.node_index,
                        epoch,
                        ?hash,
                        ?root,
                        "recovered active cut slot"
                    );
                    self.draining.insert(hash, root.clone());
                    self.decided_cut_slots.insert(hash, root);
                    self.unresolved_cut.remove(&hash);
                    continue;
                }
            }
            self.request_cut_slot(epoch, hash, "requested missing cut slot");
        }

        // Reassigning an election resolves the block/root, but clears votes and
        // tallies. Until that election reaches a terminal outcome it still needs
        // vote recovery from the other PRs.
        let stalled: Vec<_> = self
            .draining
            .iter()
            .filter_map(|(hash, root)| aec.epoch_slot_outcome(root).is_none().then_some(*hash))
            .collect();
        for hash in stalled {
            self.request_cut_slot(epoch, hash, "requested votes for draining cut slot");
        }
    }

    fn request_cut_slot(&mut self, epoch: u64, hash: BlockHash, message: &'static str) {
        let now = self.clock.now();
        if self
            .last_recovery_request
            .get(&hash)
            .is_some_and(|last| last.elapsed(now) < REPORT_REQUEST_INTERVAL)
        {
            return;
        }
        self.last_recovery_request.insert(hash, now);
        let request = ClosePayload {
            epoch,
            election_kind: 2,
            base: BlockHash::ZERO,
            target: hash,
            kind: ClosePayloadKind::SlotRequest,
        };
        self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
            &Message::ClosePayload(request),
            TrafficType::Generic,
            1.0,
        );
        tracing::warn!(
            node = self.node_index,
            epoch,
            ?hash,
            recovery = message,
            "requested cut slot recovery"
        );
    }

    fn request_active_epoch_slot_votes(&mut self, aec: &AecService) {
        let Some(epoch) = self.coordinator.closing_epoch() else {
            return;
        };
        let unresolved: Vec<_> = aec
            .epoch_slots(epoch)
            .into_iter()
            .filter_map(|(root, hash)| {
                (!aec.epoch_slot_finalized_or_timed_out(&root)).then_some(hash)
            })
            .collect();
        for hash in unresolved {
            self.request_cut_slot(epoch, hash, "requested votes for active closing-epoch slot");
        }
    }

    /// A late publish is inserted into the newly opened epoch, while its votes
    /// remain tagged with the epoch that is currently closing. Once more than f
    /// representative weight supports such a hash, recreate the missing election
    /// in the closing epoch. ElectionStarted replays the cached votes.
    fn recover_supported_epoch_slots(&mut self, aec: &AecService) {
        let Some(epoch) = self.coordinator.closing_epoch() else {
            return;
        };
        let faulty_weight = self.quorum_snapshot().faulty_weight;
        for hash in self
            .slot_vote_cache
            .supported_hashes_for_epoch(epoch, faulty_weight)
        {
            let Some(block) = self.ledger.any().get_block(&hash) else {
                self.request_cut_slot(epoch, hash, "requested supported closing-epoch slot");
                continue;
            };
            let root = block.qualified_root().with_epoch(epoch);
            if aec.has_election_for_epoch(&hash, epoch) || aec.epoch_slot_outcome(&root).is_some() {
                continue;
            }
            if aec
                .insert_for_epoch(
                    AecInsertRequest::new_manual(block, BlockPriority::new_test_instance()),
                    self.clock.now(),
                    epoch,
                )
                .is_ok()
            {
                tracing::warn!(
                    node = self.node_index,
                    epoch,
                    ?hash,
                    "recovered slot election from >f cached vote support"
                );
            }
        }
    }

    /// A certified cut obligates every replica to resolve all included slots.
    /// Keep old-epoch elections active and advertise the block and all locally
    /// stored protocol votes while the close record is waiting for them.
    fn drive_draining_slots(&mut self, aec: &AecService) {
        let epoch = self.coordinator.closing_epoch().unwrap_or_default();
        // A terminal cut-slot outcome can still change when a higher notarized
        // value is learned. Keep all decided cut slots exchanging their vote
        // histories through the record phase, not only while locally draining.
        for (hash, root) in self.decided_cut_slots.clone() {
            aec.transition_active_root(&root);

            if let Some(election) = aec.election_for_root(&root) {
                for block in election.candidate_blocks().values() {
                    self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                        &Message::Publish(Publish::new_forward(block.clone().into())),
                        TrafficType::Generic,
                        1.0,
                    );
                }
            } else if let Some(block) = self.ledger.any().get_block(&hash) {
                self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                    &Message::Publish(Publish::new_forward(block.into())),
                    TrafficType::Generic,
                    1.0,
                );
            }

            for vote in self.vote_history.all_votes_for_epoch(&root.root, epoch) {
                self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                    &Message::ConfirmAck(ConfirmAck::new_with_rebroadcasted_vote((*vote).clone())),
                    TrafficType::Generic,
                    1.0,
                );
            }
        }
    }

    fn install_closed_record(&mut self, aec: &AecService) {
        let Some(closed) = self.closed_epoch.take() else {
            return;
        };
        let finalized: HashSet<_> = closed.finalized.iter().map(|(root, _)| root).collect();
        // The record certificate is itself finality evidence. Slot notarization
        // is not required here; block validation/availability is sufficient.
        if let Some((root, hash)) = closed
            .finalized
            .iter()
            .find(|(_, hash)| self.ledger.any().get_block(hash).is_none())
            .cloned()
        {
            tracing::warn!(
                epoch = closed.epoch,
                ?root,
                ?hash,
                "record value unavailable; installation deferred"
            );
            self.request_cut_slot(closed.epoch, hash, "requested missing decided-record value");
            self.closed_epoch = Some(closed);
            return;
        }
        for (root, hash) in &closed.finalized {
            self.ledger.confirm(*hash);
            aec.apply_record_outcome(root, *hash);
        }
        for (root, _) in aec.epoch_slots(closed.epoch) {
            if finalized.contains(&root) {
                continue;
            }
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
            aec.apply_rolled_back_outcome(&root);
            tracing::info!(
                epoch = closed.epoch,
                ?root,
                "slot excluded by certified close record"
            );
        }
    }
}

impl AecTickerPlugin for CloseTransitionPlugin {
    fn run(&mut self, aec: &AecService) {
        let now = self.clock.now();
        self.classify_fixed_committee_channels();
        let Some(key) = self.local_key() else {
            return;
        };
        self.signal_close_ready();
        if !self.start_epoch_if_ready(aec, now) {
            return;
        }

        if let Some(report) = &self.local_report {
            self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                &Message::CloseReport(report.clone()),
                TrafficType::Generic,
                1.0,
            );
        }

        // A replica that has already finalized the cut must keep advertising its
        // final support until the record closes. Successor-round final votes also
        // contribute their implied notarization to a lagging proposal round.
        for cut in self.finalized_cuts.values().cloned().collect::<Vec<_>>() {
            self.publish_vote_for(&cut, cut.value, VoteType::Final, &key);
        }
        // A slower replica can enter the record election after faster replicas
        // have closed it. Retain and advertise final support so it can assemble
        // the same record certificate without relying on wall-clock expiry.
        for record in self.finalized_records.values().cloned().collect::<Vec<_>>() {
            self.publish_vote_for(&record, record.value, VoteType::Final, &key);
        }

        // Recovery requests must be served even between local close epochs. A
        // slower PR may still be draining the epoch we have already closed.
        self.drain_payloads(&key);

        if self.coordinator.closing_epoch().is_none() {
            let epoch = self.coordinator.open_epoch();
            let pending = aec.epoch_slots(epoch);
            let finalized = aec.finalized_epoch_slots(epoch);
            tracing::warn!(
                node = self.node_index,
                epoch,
                pending = ?pending,
                finalized = ?finalized,
                "local epoch contents before close report"
            );
            let Some(report) = self.coordinator.tick(
                now,
                pending.iter().map(|(_, hash)| *hash),
                finalized.clone(),
                &key,
            ) else {
                return;
            };
            // Closing freezes only fresh local support. Elections remain active
            // and routable so reports, old votes, and certificates still apply.
            aec.suppress_epoch_votes(epoch);
            debug_assert_eq!(
                aec.advance_epoch(self.confirmed_height_snapshot()),
                epoch + 1
            );
            self.local_report = Some(report.clone());
            self.local_reports.insert(epoch, report.clone());
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
        self.request_missing_reports();
        self.recover_supported_epoch_slots(aec);
        self.request_active_epoch_slot_votes(aec);
        self.refresh_record(aec, &key);
        // Close votes may be emitted before every PR has entered the matching
        // election. Keep advertising the current candidate until it obtains a
        // certificate, just as reports are advertised while being collected.
        if let Some(election) = self.coordinator.active_election() {
            self.publish_vote(&election, &key);
        }
        self.drain_votes();
        self.request_reconciliation();
        self.drive_draining_slots(aec);
        if matches!(self.coordinator.phase(), Some(ClosingPhase::DrainingCut)) {
            self.start_record_if_drained(aec, &key);
        }
        if let Some(election) = self.coordinator.active_election() {
            for (value, vote_type) in election.vote_targets() {
                self.publish_vote_for(&election, value, vote_type, &key);
            }
        }
        self.install_closed_record(aec);
        if let Some(cut) = self.pending_cut.take() {
            self.apply_cut(aec, &key, &cut);
        }

        if matches!(self.coordinator.phase(), Some(ClosingPhase::DrainingCut)) {
            self.recover_cut_data(aec);
            let epoch = self.coordinator.closing_epoch().unwrap_or_default();
            let terminated: Vec<_> = self
                .draining
                .iter()
                .filter_map(|(hash, root)| {
                    let selected_hash_confirmed = !hash.is_zero()
                        && (aec.was_recently_confirmed(hash)
                            || self.ledger.any().confirmed().block_exists(hash));
                    resolved_cut_slot_outcome(
                        aec.epoch_slot_outcome(root),
                        *hash,
                        selected_hash_confirmed,
                    )
                    .map(|outcome| (*hash, root.clone(), outcome))
                })
                .collect();
            for (hash, root, finalized) in terminated {
                tracing::warn!(node=self.node_index, epoch, ?root, ?hash, finalized=?finalized, "cut slot drained");
                self.draining.remove(&hash);
                if let Some(record) = self.coordinator.slot_terminated(root, hash, finalized) {
                    self.start_record(aec, &key, record);
                }
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn resolved_cut_slot_outcome(
    aec_outcome: Option<Option<BlockHash>>,
    selected_hash: BlockHash,
    selected_hash_confirmed: bool,
) -> Option<Option<BlockHash>> {
    if selected_hash_confirmed {
        Some(Some(selected_hash))
    } else {
        aec_outcome
    }
}

fn record_delta_fits_wire(
    upserts: &[(QualifiedRoot, BlockHash)],
    removals: &[QualifiedRoot],
) -> bool {
    record_delta_wire_size(upserts, removals) <= u16::MAX as usize
}

fn record_delta_wire_size(
    upserts: &[(QualifiedRoot, BlockHash)],
    removals: &[QualifiedRoot],
) -> usize {
    const FIXED_AND_COUNTS: usize = 1 + 8 + 1 + 32 * 2 + 4 * 2;
    const QUALIFIED_ROOT_SIZE: usize = 32 + 32 + 8;
    const RECORD_ENTRY_SIZE: usize = QUALIFIED_ROOT_SIZE + 32;
    FIXED_AND_COUNTS
        .saturating_add(upserts.len().saturating_mul(RECORD_ENTRY_SIZE))
        .saturating_add(removals.len().saturating_mul(QUALIFIED_ROOT_SIZE))
}

fn known_cut_slots(
    cut_hashes: impl IntoIterator<Item = BlockHash>,
    active_by_hash: &HashMap<BlockHash, QualifiedRoot>,
) -> HashMap<BlockHash, QualifiedRoot> {
    cut_hashes
        .into_iter()
        .filter_map(|hash| active_by_hash.get(&hash).cloned().map(|root| (hash, root)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::Root;

    #[test]
    fn record_timeout_vote_does_not_require_payload() {
        assert!(!vote_requires_record_payload(
            CloseElectionKind::Record,
            VoteType::Timeout
        ));
        assert!(vote_requires_record_payload(
            CloseElectionKind::Record,
            VoteType::First
        ));
        assert!(vote_requires_record_payload(
            CloseElectionKind::Record,
            VoteType::Final
        ));
    }

    fn root(value: u64, epoch: u64) -> QualifiedRoot {
        QualifiedRoot::new(Root::from(value), BlockHash::from(value + 100)).with_epoch(epoch)
    }

    #[test]
    fn confirmed_cut_hash_overrides_stale_non_finalized_aec_outcome() {
        let selected = BlockHash::from(1);

        assert_eq!(
            resolved_cut_slot_outcome(Some(None), selected, true),
            Some(Some(selected))
        );
        assert_eq!(
            resolved_cut_slot_outcome(Some(None), selected, false),
            Some(None)
        );
        assert_eq!(resolved_cut_slot_outcome(None, selected, false), None);
    }

    #[test]
    fn draining_preserves_all_cut_hashes_for_the_same_root() {
        let root = root(1, 7);
        let first = BlockHash::from(1);
        let second = BlockHash::from(2);
        let active_by_hash = HashMap::from([(first, root.clone()), (second, root.clone())]);

        let draining = known_cut_slots([first, second], &active_by_hash);

        assert_eq!(draining.len(), 2);
        assert_eq!(draining.get(&first), Some(&root));
        assert_eq!(draining.get(&second), Some(&root));
    }

    #[test]
    fn close_notarization_starts_a_subsequent_finalization_round() {
        let now = Timestamp::new_test_instance();
        let keys: Vec<_> = (1..=4).map(PrivateKey::from).collect();
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        let report = close
            .tick(now + Duration::from_secs(1), [], [], &keys[0])
            .unwrap();
        let proposal = close
            .add_report(report, |_| Amount::raw(1), Amount::raw(1), Amount::ZERO)
            .unwrap();
        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(5),
            faulty_weight: Amount::raw(1),
            slack_weight: Amount::ZERO,
            ..Default::default()
        };
        for key in &keys {
            let vote = CloseVote::new(
                1,
                0,
                CloseElectionKind::Cut.wire(),
                proposal.value,
                VoteType::NonFinal,
                key,
            );
            assert_eq!(close.apply_vote(&vote, Amount::raw(1), &quorum, now), None);
        }

        let finalization = close.close_notarized().unwrap();
        assert_eq!(finalization.round, 1);
        assert!(finalization.finalization_round);
        assert_eq!(finalization.local_value, proposal.value);
        assert_eq!(
            finalization.vote_targets(),
            vec![(proposal.value, VoteType::Final)]
        );

        let mut outcome = None;
        for key in &keys {
            let vote = CloseVote::new(
                1,
                1,
                CloseElectionKind::Cut.wire(),
                proposal.value,
                VoteType::Final,
                key,
            );
            outcome = close
                .apply_vote(&vote, Amount::raw(1), &quorum, now)
                .or(outcome);
        }
        assert_eq!(outcome, Some((CloseElectionKind::Cut, proposal.value)));
    }

    #[test]
    fn successor_final_votes_supply_predecessor_notarization() {
        let now = Timestamp::new_test_instance();
        let keys: Vec<_> = (1..=4).map(PrivateKey::from).collect();
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        let report = close
            .tick(now + Duration::from_secs(1), [], [], &keys[0])
            .unwrap();
        let proposal = close
            .add_report(report, |_| Amount::raw(1), Amount::raw(1), Amount::ZERO)
            .unwrap();
        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(5),
            faulty_weight: Amount::raw(1),
            slack_weight: Amount::ZERO,
            ..Default::default()
        };

        for key in &keys {
            let final_vote = CloseVote::new(
                proposal.epoch,
                proposal.round + 1,
                proposal.kind.wire(),
                proposal.value,
                VoteType::Final,
                key,
            );
            assert!(close.accepts_successor_final_evidence(&final_vote));
            assert!(close.apply_successor_final_evidence(
                &final_vote,
                Amount::raw(1),
                &quorum,
                now,
            ));
        }

        let finalization = close.close_notarized().unwrap();
        assert_eq!(finalization.round, proposal.round + 1);
        assert_eq!(finalization.value, proposal.value);
        assert!(finalization.finalization_round);
    }

    #[test]
    fn same_round_final_votes_catch_up_a_timeout_proposal_round() {
        let now = Timestamp::new_test_instance();
        let keys: Vec<_> = (1..=4).map(PrivateKey::from).collect();
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        let report = close
            .tick(now + Duration::from_secs(1), [], [], &keys[0])
            .unwrap();
        let proposal = close
            .add_report(report, |_| Amount::raw(1), Amount::raw(1), Amount::ZERO)
            .unwrap();
        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(5),
            faulty_weight: Amount::raw(1),
            slack_weight: Amount::ZERO,
            ..Default::default()
        };

        if let ClosingPhase::ElectingCut(election) = &mut close.closing.as_mut().unwrap().phase {
            election.add_candidate(BlockHash::from(999));
        }
        for key in &keys {
            let timeout = CloseVote::new(
                proposal.epoch,
                proposal.round,
                proposal.kind.wire(),
                proposal.value,
                VoteType::Timeout,
                key,
            );
            close.apply_vote(&timeout, Amount::raw(1), &quorum, now);
        }
        let timeout_round = close.close_timed_out().unwrap();
        assert_eq!(timeout_round.round, 1);
        assert!(!timeout_round.finalization_round);

        for key in &keys {
            let final_vote = CloseVote::new(
                proposal.epoch,
                timeout_round.round,
                proposal.kind.wire(),
                proposal.value,
                VoteType::Final,
                key,
            );
            assert!(close.accepts_same_round_final_evidence(&final_vote));
            assert!(close.apply_same_round_final_evidence(
                &final_vote,
                Amount::raw(1),
                &quorum,
                now,
            ));
        }

        let finalization = close.close_notarized().unwrap();
        assert_eq!(finalization.round, timeout_round.round);
        assert_eq!(finalization.value, proposal.value);
        assert!(finalization.finalization_round);
    }

    #[test]
    fn proposal_replay_preserves_same_round_final_votes_for_finalization() {
        let key = PrivateKey::from(1);
        let value = BlockHash::from(7);
        let mut cache = CloseVoteCache::default();
        cache.insert(CloseVote::new(
            1,
            1,
            CloseElectionKind::Record.wire(),
            value,
            VoteType::Final,
            &key,
        ));

        let proposal = CloseElection::new(CloseElectionKind::Record, 1, 1, value);
        assert!(cache.take(&proposal).is_empty());

        let mut finalization = proposal;
        finalization.finalization_round = true;
        let votes = cache.take(&finalization);
        assert_eq!(votes.len(), 1);
        assert_eq!(votes[0].vote_type, VoteType::Final);
    }

    #[test]
    fn opaque_notarized_value_does_not_discard_the_valid_reconciliation_base() {
        let now = Timestamp::new_test_instance();
        let keys: Vec<_> = (1..=4).map(PrivateKey::from).collect();
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        let report = close
            .tick(now + Duration::from_secs(1), [], [], &keys[0])
            .unwrap();
        let proposal = close
            .add_report(report, |_| Amount::raw(1), Amount::raw(1), Amount::ZERO)
            .unwrap();
        let opaque = BlockHash::from(999);
        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(5),
            faulty_weight: Amount::raw(1),
            slack_weight: Amount::ZERO,
            ..Default::default()
        };
        for key in &keys {
            let vote = CloseVote::new(
                1,
                0,
                CloseElectionKind::Cut.wire(),
                opaque,
                VoteType::NonFinal,
                key,
            );
            close.apply_vote(&vote, Amount::raw(1), &quorum, now);
        }

        assert!(close.close_notarized().is_none());
        let active = close.active_election().unwrap();
        assert_eq!(active.local_value, proposal.value);
        assert!(!active.finalization_round);
    }

    #[test]
    fn future_reports_are_retained_until_their_epoch_closes() {
        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
        ];
        let mut cache = CloseReportCache::default();
        cache.insert(CloseReport::new(2, [BlockHash::from(20)], &keys[0]));
        cache.insert(CloseReport::new(3, [BlockHash::from(30)], &keys[1]));
        cache.insert(CloseReport::new(3, [BlockHash::from(31)], &keys[2]));

        assert_eq!(cache.reports(2).len(), 1);
        assert_eq!(cache.reports(3).len(), 2);

        cache.remove_closed(2);
        assert!(cache.reports(2).is_empty());
        assert_eq!(cache.reports(3).len(), 2);

        cache.remove_closed(3);
        assert!(cache.reports(3).is_empty());
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
    fn empty_epoch_starts_closing_at_deadline() {
        let now = Timestamp::new_test_instance();
        let duration = Duration::from_secs(5);
        let key = PrivateKey::from(1);
        let mut close = CloseCoordinator::new(now, duration);

        let report = close.tick(now + duration, [], [], &key).unwrap();
        assert_eq!(report.epoch, 1);
        assert!(report.pending.is_empty());
        assert!(report.validate());
        assert_eq!(close.open_epoch(), 2);
        assert_eq!(close.latest_closed_epoch(), 0);
        assert_eq!(close.closing_epoch(), Some(1));
        assert!(
            close
                .tick(now + duration * 2, [BlockHash::from(1)], [], &key)
                .is_none()
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
        let included_hash = BlockHash::from(201);
        let excluded_hash = BlockHash::from(202);
        let mut close = CloseCoordinator::new(now, duration);

        assert!(close.tick(now, [], [], &keys[0]).is_none());
        let local_report = close
            .tick(
                now + duration,
                [included_hash, excluded_hash],
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
                CloseReport::new(1, [included_hash], &keys[1]),
                |reporter| weights.get(reporter).copied().unwrap_or_default(),
                Amount::raw(3),
                Amount::raw(1),
            )
            .unwrap();
        assert_eq!(cut.kind, CloseElectionKind::Cut);

        let excluded_roots = close
            .cut_finalized(
                cut.value,
                [
                    (included.clone(), included_hash),
                    (excluded.clone(), excluded_hash),
                ],
            )
            .unwrap();
        assert_eq!(excluded_roots, vec![excluded]);

        let record = close
            .slot_terminated(included.clone(), included_hash, Some(included_hash))
            .unwrap();
        assert_eq!(record.kind, CloseElectionKind::Record);
        assert!(close.record_finalized(record.value).is_some());
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
        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(1),
            faulty_weight: Amount::ZERO,
            slack_weight: Amount::ZERO,
            ..Default::default()
        };

        let updated = close
            .refresh_record([(learned.clone(), learned_hash)], &quorum)
            .unwrap();
        let updated_hash = close_record_hash(
            1,
            BlockHash::ZERO,
            &[(first.clone(), first_hash), (learned, learned_hash)],
        );
        assert_eq!(updated.local_value, initial.value);
        assert!(updated.candidates.contains(&updated_hash));

        let timeout = CloseVote::new(
            1,
            0,
            CloseElectionKind::Record.wire(),
            initial.value,
            VoteType::Timeout,
            &key,
        );
        assert_eq!(
            close.apply_vote(&timeout, Amount::raw(1), &quorum, now),
            None
        );
        let next = close.close_timed_out().unwrap();
        assert_eq!(next.round, 1);
        assert_eq!(next.local_value, updated_hash);
    }

    #[test]
    fn finalized_cut_discards_deferred_cut_value_before_record_phase() {
        let now = Timestamp::new_test_instance();
        let key = PrivateKey::from(1);
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        close
            .tick(now + Duration::from_secs(1), [], [], &key)
            .unwrap();
        let cut = close
            .add_report(
                CloseReport::new(1, [], &key),
                |_| Amount::raw(1),
                Amount::raw(1),
                Amount::ZERO,
            )
            .unwrap();
        let stale_cut_hash = BlockHash::from(999);
        close.closing.as_mut().unwrap().deferred_cut_value = Some(stale_cut_hash);

        close.cut_finalized(cut.value, []).unwrap();
        let record = close.finish_empty_drain().unwrap();

        assert_eq!(close.closing.as_ref().unwrap().deferred_cut_value, None);
        assert_ne!(record.local_value, stale_cut_hash);
        assert!(close.current_record_payload().is_some());
    }

    #[test]
    fn record_refresh_replaces_stale_slot_outcome() {
        let now = Timestamp::new_test_instance();
        let key = PrivateKey::from(1);
        let slot = root(1, 1);
        let stale_hash = BlockHash::from(1);
        let selected_hash = BlockHash::from(2);
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));

        let report = close
            .tick(
                now + Duration::from_secs(1),
                [],
                [(slot.clone(), stale_hash)],
                &key,
            )
            .unwrap();
        let cut = close
            .add_report(report, |_| Amount::raw(1), Amount::raw(1), Amount::ZERO)
            .unwrap();
        close.cut_finalized(cut.value, []).unwrap();
        close.finish_empty_drain().unwrap();

        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(1),
            faulty_weight: Amount::ZERO,
            slack_weight: Amount::ZERO,
            ..Default::default()
        };
        close
            .refresh_record([(slot.clone(), selected_hash)], &quorum)
            .unwrap();

        let (_, payload) = close.current_record_payload().unwrap();
        assert_eq!(payload, vec![(slot, selected_hash)]);
    }

    #[test]
    fn reconciled_record_payloads_converge_by_monotonic_union() {
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
        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(1),
            faulty_weight: Amount::ZERO,
            slack_weight: Amount::ZERO,
            ..Default::default()
        };

        let payload = vec![first.clone(), second];
        let target = close_record_hash(1, BlockHash::ZERO, &payload);
        assert!(
            close
                .admit_record_payload(record.value, target, payload.clone(), &quorum)
                .is_some()
        );

        // Replaying an older/subset record must not remove locally validated
        // entries or change the canonical proposal.
        let subset = Vec::new();
        let subset_target = close_record_hash(1, BlockHash::ZERO, &subset);
        assert!(
            close
                .admit_record_payload(target, subset_target, subset, &quorum)
                .is_some()
        );
        let (_, current) = close.current_record_payload().unwrap();
        assert_eq!(current, payload);

        // Delivery order does not matter: a replica starting from the subset
        // derives the same union hash after learning the larger record.
        let expected = target;
        assert_eq!(close_record_hash(1, BlockHash::ZERO, &current), expected);
    }

    #[test]
    fn reconciled_conflicting_record_remains_decidable_without_overwriting_local_outcome() {
        let now = Timestamp::new_test_instance();
        let key = PrivateKey::from(1);
        let slot = root(1, 1);
        let local = (slot.clone(), BlockHash::from(101));
        let conflicting = (slot, BlockHash::from(202));
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        let report = close
            .tick(now + Duration::from_secs(1), [], [local], &key)
            .unwrap();
        let cut = close
            .add_report(report, |_| Amount::raw(1), Amount::raw(1), Amount::ZERO)
            .unwrap();
        close.cut_finalized(cut.value, []).unwrap();
        let local_record = close.finish_empty_drain().unwrap();
        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(1),
            faulty_weight: Amount::ZERO,
            slack_weight: Amount::ZERO,
            ..Default::default()
        };
        let payload = vec![conflicting];
        let target = close_record_hash(1, BlockHash::ZERO, &payload);

        let election = close
            .admit_record_payload(BlockHash::ZERO, target, payload.clone(), &quorum)
            .unwrap();
        assert!(election.candidates.contains(&target));
        assert_eq!(election.pending_value, None);
        assert_eq!(election.local_value, local_record.local_value);
        let closing = close.closing.as_ref().unwrap();
        assert_eq!(closing.finalized, vec![(root(1, 1), BlockHash::from(101))]);
        assert_eq!(closing.record_candidates[&target], payload);
    }

    #[test]
    fn retroactively_validated_record_rechecks_stored_final_votes() {
        let now = Timestamp::new_test_instance();
        let key = PrivateKey::from(1);
        let initial = BlockHash::from(1);
        let learned = BlockHash::from(2);
        let quorum = QuorumSnapshot {
            total_weight: Amount::raw(1),
            faulty_weight: Amount::ZERO,
            slack_weight: Amount::ZERO,
            ..Default::default()
        };
        let mut election = CloseElection::new(CloseElectionKind::Record, 1, 1, initial);
        election.finalization_round = true;

        election
            .apply_vote(
                key.public_key(),
                learned,
                VoteType::Final,
                Amount::raw(1),
                &quorum,
                now,
            )
            .unwrap();
        assert!(!election.is_finalized());

        assert!(election.add_candidate(learned));
        election.update_outcome(&quorum);
        assert!(election.is_finalized());
        assert_eq!(election.value, learned);
    }

    #[test]
    fn cut_delta_transmits_only_mutations_and_reconstructs_target() {
        let base = vec![BlockHash::from(1), BlockHash::from(2), BlockHash::from(3)];
        let target = vec![BlockHash::from(2), BlockHash::from(3), BlockHash::from(4)];
        let (additions, removals) = cut_delta(&base, &target);

        assert_eq!(additions, vec![BlockHash::from(4)]);
        assert_eq!(removals, vec![BlockHash::from(1)]);
        assert_eq!(apply_cut_delta(&base, &additions, &removals), Some(target));
    }

    #[test]
    fn record_delta_transmits_only_mutations_and_reconstructs_target() {
        let unchanged = (root(1, 1), BlockHash::from(101));
        let replaced_root = root(2, 1);
        let removed = (root(3, 1), BlockHash::from(103));
        let added = (root(4, 1), BlockHash::from(104));
        let base = vec![
            unchanged.clone(),
            (replaced_root.clone(), BlockHash::from(102)),
            removed.clone(),
        ];
        let target = vec![
            unchanged,
            (replaced_root.clone(), BlockHash::from(202)),
            added.clone(),
        ];
        let (upserts, removals) = record_delta(&base, &target);

        assert_eq!(upserts, vec![(replaced_root, BlockHash::from(202)), added]);
        assert_eq!(removals, vec![removed.0]);
        assert_eq!(apply_record_delta(&base, &upserts, &removals), Some(target));
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

        let next_value = BlockHash::from(10);
        let ClosingPhase::ElectingCut(election) = &mut close.closing.as_mut().unwrap().phase else {
            panic!("expected cut election");
        };
        election.known_values.insert(next_value);
        election.candidates.insert(next_value);
        election.pending_value = Some(next_value);
        election.value_updated = false;

        let next = close.close_timed_out().unwrap();
        assert_eq!(next.round, 1);
        assert_eq!(next.local_value, next_value);
    }

    #[test]
    fn late_report_defers_new_cut_until_round_one() {
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
        let first_hash = BlockHash::from(101);
        let late_hash = BlockHash::from(102);
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));

        let first_report = close
            .tick(
                now + Duration::from_secs(1),
                [first_hash, late_hash],
                [],
                &keys[0],
            )
            .unwrap();
        assert!(
            close
                .add_report(
                    first_report,
                    |reporter| weights.get(reporter).copied().unwrap_or_default(),
                    Amount::raw(3),
                    Amount::raw(1),
                )
                .is_none()
        );
        let round_zero = close
            .add_report(
                CloseReport::new(1, [first_hash], &keys[1]),
                |reporter| weights.get(reporter).copied().unwrap_or_default(),
                Amount::raw(3),
                Amount::raw(1),
            )
            .unwrap();

        assert_eq!(round_zero.round, 0);
        assert!(
            close
                .add_report(
                    CloseReport::new(1, [late_hash], &keys[2]),
                    |reporter| weights.get(reporter).copied().unwrap_or_default(),
                    Amount::raw(3),
                    Amount::raw(1),
                )
                .is_none()
        );
        assert_eq!(close.active_election().unwrap(), round_zero);
        assert!(
            close
                .cut_payload_hashes()
                .contains(&close_cut_hash(1, &[first_hash, late_hash]))
        );

        let round_one = close.close_timed_out().unwrap();
        assert_eq!(round_one.round, 1);
        assert_ne!(round_one.local_value, round_zero.local_value);
        assert_eq!(
            round_one.local_value,
            close_cut_hash(1, &[first_hash, late_hash])
        );
        assert!(round_one.candidates.contains(&round_one.local_value));
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
    fn new_report_evidence_is_deferred_until_next_cut_round() {
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
        let candidate = BlockHash::from(44);
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
                    CloseReport::new(1, [candidate], &keys[0]),
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
        assert!(
            close
                .add_report(
                    CloseReport::new(1, [candidate], &keys[2]),
                    weight,
                    Amount::raw(3),
                    Amount::raw(1),
                )
                .is_none()
        );
        assert_eq!(close.active_election().unwrap(), first);

        let second = close.close_timed_out().unwrap();
        assert_eq!(second.round, 1);
        assert_ne!(first.value, second.value);
        assert_eq!(second.value, close_cut_hash(1, &[candidate]));
        assert!(second.candidates.contains(&second.value));
    }
}
