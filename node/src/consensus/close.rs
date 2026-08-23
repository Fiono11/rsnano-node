use std::{
    any::Any,
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, mpsc::Receiver},
    time::Duration,
};

use rsnano_ledger::RepWeightCache;
use rsnano_messages::{CloseReport, CloseVote, Message};
use rsnano_network::TrafficType;
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
    candidates: HashSet<BlockHash>,
    votes: HashMap<PublicKey, VoteSummary>,
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
            candidates: HashSet::from([value]),
            votes: HashMap::new(),
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
        if !self.candidates.contains(&value) {
            return Err(VoteError::Indeterminate);
        }
        let mut vote = VoteSummary::new(voter, value, UnixMillisTimestamp::new(0), now);
        vote.apply_phase(
            vote_type,
            value,
            UnixMillisTimestamp::new(vote_type as u64 + 1),
            now,
        )?;
        vote.weight = weight;
        self.votes.insert(voter, vote);
        self.update_outcome(quorum);
        Ok(())
    }

    fn update_outcome(&mut self, quorum: &QuorumSnapshot) {
        let timeout: Amount = self
            .votes
            .values()
            .filter(|vote| vote.timeout)
            .map(|vote| vote.weight)
            .sum();
        let certificate = quorum.total_weight - quorum.faulty_weight - quorum.slack_weight;
        for candidate in self.candidates.iter().copied() {
            let first: Amount = self
                .votes
                .values()
                .filter(|vote| vote.first == Some(candidate))
                .map(|vote| vote.weight)
                .sum();
            let final_weight: Amount = self
                .votes
                .values()
                .filter(|vote| vote.final_vote == Some(candidate))
                .map(|vote| vote.weight)
                .sum();
            if first >= quorum.total_weight - quorum.slack_weight || final_weight >= certificate {
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
        self.candidates.insert(value)
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
}

impl From<&CloseVote> for CloseVoteCacheKey {
    fn from(vote: &CloseVote) -> Self {
        Self {
            kind: vote.kind,
            epoch: vote.epoch,
            round: vote.round,
            value: vote.value,
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
        if !self.entries.contains_key(&key) && self.entries.len() >= Self::MAX_ELECTIONS {
            return;
        }
        let voters = self.entries.entry(key).or_default();
        if voters.len() < Self::MAX_VOTERS || voters.contains_key(&vote.voter) {
            voters.insert(vote.voter, vote);
        }
    }

    fn take(&mut self, election: &CloseElection) -> Vec<CloseVote> {
        self.entries
            .remove(&CloseVoteCacheKey {
                kind: election.kind.wire(),
                epoch: election.epoch,
                round: election.round,
                value: election.value,
            })
            .map(|votes| votes.into_values().collect())
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

    pub fn closing_epoch(&self) -> Option<u64> {
        self.closing.as_ref().map(|close| close.epoch)
    }

    pub fn latest_closed_epoch(&self) -> u64 {
        self.latest_closed_epoch
    }

    pub fn phase(&self) -> Option<&ClosingPhase> {
        self.closing.as_ref().map(|close| &close.phase)
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
            || vote.value != election.value
        {
            return None;
        }
        election
            .apply_vote(vote.voter, vote.value, vote.vote_type, weight, quorum, now)
            .ok()?;
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
            && vote.value == election.value
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
            finalized: finalized.into_iter().collect(),
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
        let next = CloseElection::new(
            current.kind,
            current.epoch,
            current.round + 1,
            current.value,
        );
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
    draining: HashMap<QualifiedRoot, BlockHash>,
    report_rx: Receiver<CloseReport>,
    vote_rx: Receiver<CloseVote>,
    flooder: Mutex<MessageFlooder>,
    local_report: Option<CloseReport>,
    pending_cut: Option<CloseElection>,
    vote_cache: CloseVoteCache,
}

impl CloseTransitionPlugin {
    pub fn new(
        epoch_duration: Duration,
        clock: Arc<SteadyClock>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        rep_weights: Arc<RepWeightCache>,
        rep_tracker: Arc<RepresentativeTracker>,
        report_rx: Receiver<CloseReport>,
        vote_rx: Receiver<CloseVote>,
        flooder: MessageFlooder,
    ) -> Self {
        let start_delay = std::env::var("NANO_RAI_EPOCH_START_DELAY_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
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
            draining: HashMap::new(),
            report_rx,
            vote_rx,
            flooder: Mutex::new(flooder),
            local_report: None,
            pending_cut: None,
            vote_cache: CloseVoteCache::default(),
        }
    }

    fn local_key(&self) -> Option<PrivateKey> {
        let mut keys = Vec::new();
        self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
        keys.into_iter().next()
    }

    fn publish_vote(&mut self, election: &CloseElection, key: &PrivateKey) {
        let vote = CloseVote::new(
            election.epoch,
            election.round,
            election.kind.wire(),
            election.value,
            VoteType::First,
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
        let quorum = self.rep_tracker.quorum_snapshot();
        let Some((kind, value)) = self.coordinator.apply_vote(
            &vote,
            self.rep_weights.weight(&vote.voter),
            &quorum,
            self.clock.now(),
        ) else {
            return;
        };
        match kind {
            CloseElectionKind::Cut => {
                self.vote_cache
                    .remove_obsolete(vote.epoch, CloseElectionKind::Cut, u32::MAX);
                self.pending_cut = Some(CloseElection::new(
                    CloseElectionKind::Cut,
                    vote.epoch,
                    vote.round,
                    value,
                ))
            }
            CloseElectionKind::Record => {
                if self.coordinator.record_finalized(value) {
                    self.vote_cache.remove_obsolete(
                        vote.epoch,
                        CloseElectionKind::Record,
                        u32::MAX,
                    );
                    tracing::warn!(epoch = vote.epoch, "epoch closed");
                }
            }
        }
    }

    fn apply_report(&mut self, _aec: &AecService, key: &PrivateKey, report: CloseReport) {
        let quorum = self.rep_tracker.quorum_snapshot();
        if let Some(cut) = self.coordinator.add_report(
            report,
            |reporter| self.rep_weights.weight(reporter),
            quorum.total_weight,
            quorum.faulty_weight,
        ) {
            self.local_report = None;
            tracing::warn!(epoch = cut.epoch, round = cut.round, value = ?cut.value, "close cut started");
            self.publish_vote(&cut, key);
            self.replay_cached_votes(&cut);
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
        tracing::info!(
            epoch = record.epoch,
            round = record.round,
            "close record started"
        );
        self.publish_vote(&record, key);
        self.replay_cached_votes(&record);
    }

    fn apply_cut(&mut self, aec: &AecService, key: &PrivateKey, cut: &CloseElection) {
        let active = aec.epoch_slots(cut.epoch);
        let roots = active.iter().map(|(root, _)| root.clone());
        let Some(excluded) = self.coordinator.cut_finalized(cut.value, roots) else {
            return;
        };
        for root in excluded {
            aec.exclude_by_cut(&root);
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
        self.start_record_if_drained(key);
    }
}

impl AecTickerPlugin for CloseTransitionPlugin {
    fn run(&mut self, aec: &AecService) {
        let Some(key) = self.local_key() else {
            return;
        };
        let now = self.clock.now();

        if let Some(report) = &self.local_report {
            self.flooder.lock().unwrap().flood_prs_and_some_non_prs(
                &Message::CloseReport(report.clone()),
                TrafficType::Generic,
                1.0,
            );
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
        self.drain_votes();
        if let Some(cut) = self.pending_cut.take() {
            self.apply_cut(aec, &key, &cut);
        }

        if matches!(self.coordinator.phase(), Some(ClosingPhase::DrainingCut)) {
            let terminated: Vec<_> = self
                .draining
                .iter()
                .filter(|(root, _)| !aec.is_active_root(root))
                .map(|(root, hash)| (root.clone(), *hash))
                .collect();
            for (root, hash) in terminated {
                self.draining.remove(&root);
                let finalized = aec.was_recently_confirmed(&hash).then_some(hash);
                if let Some(record) = self.coordinator.slot_terminated(root, finalized) {
                    tracing::info!(
                        epoch = record.epoch,
                        round = record.round,
                        "close record started"
                    );
                    self.publish_vote(&record, &key);
                    self.replay_cached_votes(&record);
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
    fn timeout_certificate_advances_close_round() {
        let now = Timestamp::new_test_instance();
        let key = PrivateKey::from(1);
        let weights = HashMap::from([(key.public_key(), Amount::raw(1))]);
        let mut close = CloseCoordinator::new(now, Duration::from_secs(1));
        let report = close
            .tick(now + Duration::from_secs(1), [], [], &key)
            .unwrap();
        close
            .add_report(
                report,
                |reporter| weights.get(reporter).copied().unwrap_or_default(),
                Amount::raw(1),
                Amount::ZERO,
            )
            .unwrap();
        let next = close.close_timed_out().unwrap();
        assert_eq!(next.kind, CloseElectionKind::Cut);
        assert_eq!(next.round, 1);
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
            .tick(now + Duration::from_secs(1), [], [], &keys[0])
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
