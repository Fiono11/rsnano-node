use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    sync::{Arc, RwLock},
};

use rsnano_types::{
    PublicKey, RaiElectionId, RaiElectionValue, RaiEpoch, RaiSlot, RaiVote, RaiVoteKind, VoteError,
};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

use super::{
    NoopRaiStatePersistence, RaiCommittee, RaiCommitteeSet, RaiStatePersistence, VisibleSlots,
};

pub struct RaiActiveElections {
    data: RwLock<RaiActiveElectionsData>,
    persistence: Arc<dyn RaiStatePersistence>,
}

impl RaiActiveElections {
    pub fn new() -> Self {
        Self::with_persistence(Arc::new(NoopRaiStatePersistence))
    }

    pub fn with_persistence(persistence: Arc<dyn RaiStatePersistence>) -> Self {
        Self {
            data: RwLock::new(RaiActiveElectionsData::default()),
            persistence,
        }
    }

    pub fn from_snapshot(snapshot: RaiActiveElectionsSnapshot) -> Self {
        Self::from_snapshot_with_persistence(snapshot, Arc::new(NoopRaiStatePersistence))
    }

    pub fn from_snapshot_with_persistence(
        snapshot: RaiActiveElectionsSnapshot,
        persistence: Arc<dyn RaiStatePersistence>,
    ) -> Self {
        Self {
            data: RwLock::new(RaiActiveElectionsData::from_snapshot(snapshot)),
            persistence,
        }
    }

    pub fn snapshot(&self) -> RaiActiveElectionsSnapshot {
        self.data.read().unwrap().snapshot()
    }

    pub fn insert(&self, election_id: RaiElectionId) -> Result<(), RaiElectionInsertError> {
        let snapshot = {
            let mut guard = self.data.write().unwrap();
            if guard.stopped {
                return Err(RaiElectionInsertError::Stopped);
            }

            if guard.elections.contains_key(&election_id) {
                return Err(RaiElectionInsertError::Duplicate);
            }

            guard
                .elections
                .insert(election_id.clone(), RaiElection::new(election_id));
            guard.snapshot()
        };

        self.persistence.save_active_elections(&snapshot);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.data.read().unwrap().elections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, election_id: &RaiElectionId) -> bool {
        self.data
            .read()
            .unwrap()
            .elections
            .contains_key(election_id)
    }

    pub fn is_active(&self, election_id: &RaiElectionId) -> bool {
        self.data
            .read()
            .unwrap()
            .elections
            .get(election_id)
            .is_some_and(|election| election.status() == RaiElectionStatus::Active)
    }

    pub fn election(&self, election_id: &RaiElectionId) -> Option<RaiElection> {
        self.data
            .read()
            .unwrap()
            .elections
            .get(election_id)
            .cloned()
    }

    pub fn unfinished_slots(&self, epoch: u64) -> Vec<RaiSlot> {
        self.data
            .read()
            .unwrap()
            .elections
            .values()
            .filter_map(|election| match election.id() {
                RaiElectionId::Slot {
                    slot,
                    epoch: slot_epoch,
                } if *slot_epoch == epoch && election.status() == RaiElectionStatus::Active => {
                    Some(*slot)
                }
                _ => None,
            })
            .collect()
    }

    pub fn discard_slots_outside_cut(&self, epoch: RaiEpoch, cut: &VisibleSlots) -> bool {
        let snapshot = {
            let mut guard = self.data.write().unwrap();
            let before = guard.elections.len();
            guard.elections.retain(|election_id, _| match election_id {
                RaiElectionId::Slot {
                    slot,
                    epoch: slot_epoch,
                } if *slot_epoch == epoch => cut.contains(slot),
                _ => true,
            });

            if guard.elections.len() == before {
                return false;
            }

            guard.snapshot()
        };

        self.persistence.save_active_elections(&snapshot);
        true
    }

    pub fn apply_vote(
        &self,
        vote: &RaiVote,
        committees: &RaiCommitteeSet,
    ) -> Result<(), VoteError> {
        let snapshot = {
            let mut guard = self.data.write().unwrap();
            let Some(election) = guard.elections.get_mut(&vote.election_id) else {
                return Err(VoteError::Indeterminate);
            };

            election.apply_vote(vote, committees)?;
            guard.snapshot()
        };

        self.persistence.save_active_elections(&snapshot);
        Ok(())
    }

    pub fn stop(&self) {
        self.data.write().unwrap().stopped = true;
    }
}

impl Default for RaiActiveElections {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainerInfoProvider for RaiActiveElections {
    fn container_info(&self) -> ContainerInfo {
        [(
            "elections",
            self.data.read().unwrap().elections.len(),
            size_of::<RaiElectionId>() + size_of::<RaiElection>(),
        )]
        .into()
    }
}

#[derive(Default)]
struct RaiActiveElectionsData {
    stopped: bool,
    elections: HashMap<RaiElectionId, RaiElection>,
}

impl RaiActiveElectionsData {
    fn from_snapshot(snapshot: RaiActiveElectionsSnapshot) -> Self {
        let mut elections = HashMap::new();
        for election in snapshot.elections {
            elections.insert(election.id.clone(), RaiElection::from_snapshot(election));
        }

        Self {
            stopped: false,
            elections,
        }
    }

    fn snapshot(&self) -> RaiActiveElectionsSnapshot {
        let mut elections: Vec<_> = self.elections.values().map(RaiElection::snapshot).collect();
        elections.sort_by_key(|election| serialized_election_id(&election.id));

        RaiActiveElectionsSnapshot { elections }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RaiActiveElectionsSnapshot {
    pub elections: Vec<RaiElectionSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiElectionSnapshot {
    pub id: RaiElectionId,
    pub status: RaiElectionStatus,
    pub vote_states: Vec<RaiVoteStateSnapshot>,
    pub tallies: Vec<RaiTallySnapshot>,
    pub notarization_tallies: Vec<RaiTallySnapshot>,
    pub final_tallies: Vec<RaiTallySnapshot>,
    pub winner: Option<RaiElectionValue>,
    pub confirmed_value: Option<RaiElectionValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiElectionOutcome {
    Timeout,
    Notarized(RaiElectionValue),
    Fast(RaiElectionValue),
    Final(RaiElectionValue),
    SafetyFault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiTallySnapshot {
    pub value: RaiElectionValue,
    pub per_committee: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiElectionInsertError {
    Stopped,
    Duplicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiElectionStatus {
    Active,
    Confirmed,
}

#[derive(Clone)]
pub struct RaiElection {
    id: RaiElectionId,
    status: RaiElectionStatus,
    vote_states: HashMap<RaiCommitteeVoteKey, RaiVoteState>,
    tallies: HashMap<RaiElectionValue, RaiCommitteeVoteCounts>,
    notarization_tallies: HashMap<RaiElectionValue, RaiCommitteeVoteCounts>,
    final_tallies: HashMap<RaiElectionValue, RaiCommitteeVoteCounts>,
    winner: Option<RaiElectionValue>,
    confirmed_value: Option<RaiElectionValue>,
}

impl RaiElection {
    pub fn new(id: RaiElectionId) -> Self {
        Self {
            id,
            status: RaiElectionStatus::Active,
            vote_states: HashMap::new(),
            tallies: HashMap::new(),
            notarization_tallies: HashMap::new(),
            final_tallies: HashMap::new(),
            winner: None,
            confirmed_value: None,
        }
    }

    pub fn from_snapshot(snapshot: RaiElectionSnapshot) -> Self {
        Self {
            id: snapshot.id,
            status: snapshot.status,
            vote_states: snapshot
                .vote_states
                .into_iter()
                .map(|vote_state| {
                    (
                        RaiCommitteeVoteKey {
                            voter: vote_state.voter,
                            committee_index: vote_state.committee_index,
                        },
                        RaiVoteState::from_snapshot(vote_state),
                    )
                })
                .collect(),
            tallies: snapshot
                .tallies
                .into_iter()
                .map(|tally| {
                    (
                        tally.value,
                        RaiCommitteeVoteCounts::from_snapshot(tally.per_committee),
                    )
                })
                .collect(),
            notarization_tallies: snapshot
                .notarization_tallies
                .into_iter()
                .map(|tally| {
                    (
                        tally.value,
                        RaiCommitteeVoteCounts::from_snapshot(tally.per_committee),
                    )
                })
                .collect(),
            final_tallies: snapshot
                .final_tallies
                .into_iter()
                .map(|tally| {
                    (
                        tally.value,
                        RaiCommitteeVoteCounts::from_snapshot(tally.per_committee),
                    )
                })
                .collect(),
            winner: snapshot.winner,
            confirmed_value: snapshot.confirmed_value,
        }
    }

    pub fn snapshot(&self) -> RaiElectionSnapshot {
        let mut vote_states: Vec<_> = self
            .vote_states
            .iter()
            .map(|(key, state)| state.snapshot(*key))
            .collect();
        vote_states.sort_by_key(|vote| (vote.voter, vote.committee_index));

        RaiElectionSnapshot {
            id: self.id.clone(),
            status: self.status,
            vote_states,
            tallies: snapshot_tallies(&self.tallies),
            notarization_tallies: snapshot_tallies(&self.notarization_tallies),
            final_tallies: snapshot_tallies(&self.final_tallies),
            winner: self.winner.clone(),
            confirmed_value: self.confirmed_value.clone(),
        }
    }

    pub fn id(&self) -> &RaiElectionId {
        &self.id
    }

    pub fn status(&self) -> RaiElectionStatus {
        self.status
    }

    pub fn voters(&self) -> Vec<PublicKey> {
        let mut voters: Vec<_> = self
            .vote_states
            .keys()
            .map(|key| key.voter)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        voters.sort();
        voters
    }

    pub fn vote_state_snapshots(&self) -> Vec<RaiVoteStateSnapshot> {
        self.snapshot().vote_states
    }

    pub fn tally(&self, value: &RaiElectionValue) -> usize {
        self.tallies
            .get(value)
            .map(RaiCommitteeVoteCounts::total)
            .unwrap_or_default()
    }

    pub fn final_tally(&self, value: &RaiElectionValue) -> usize {
        self.final_tallies
            .get(value)
            .map(RaiCommitteeVoteCounts::total)
            .unwrap_or_default()
    }

    pub fn winner(&self) -> Option<&RaiElectionValue> {
        self.winner.as_ref()
    }

    pub fn confirmed_value(&self) -> Option<&RaiElectionValue> {
        self.confirmed_value.as_ref()
    }

    pub fn fast_value(&self, committees: &RaiCommitteeSet) -> Option<RaiElectionValue> {
        match self.merged_outcome(committees) {
            Some(RaiElectionOutcome::Fast(value)) => Some(value),
            _ => None,
        }
    }

    pub fn timeout_ready_committee_indexes(
        &self,
        voter: &PublicKey,
        committees: &RaiCommitteeSet,
    ) -> Vec<usize> {
        committees
            .iter()
            .enumerate()
            .filter_map(|(committee_index, committee)| {
                if committee.contains(voter)
                    && self.voter_can_timeout(*voter, committee_index)
                    && self.timeout_ready(committee_index, committee)
                {
                    Some(committee_index)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn merged_outcome(&self, committees: &RaiCommitteeSet) -> Option<RaiElectionOutcome> {
        if committees.is_empty() {
            return None;
        }

        let mut merged = None;
        for (committee_index, committee) in committees.iter().enumerate() {
            let local = self.local_outcome(committee_index, committee)?;
            merged = Some(match merged {
                None => local,
                Some(existing) => merge_outcomes(existing, local),
            });
        }
        merged
    }

    fn apply_vote(
        &mut self,
        vote: &RaiVote,
        committees: &RaiCommitteeSet,
    ) -> Result<(), VoteError> {
        if !self.accepts_vote(vote) {
            return Err(VoteError::Invalid);
        }

        let committee_indexes = committees.scoped_committee_indexes_for(&vote.voter, vote.scope);
        if committee_indexes.is_empty() {
            return Err(VoteError::Indeterminate);
        }

        for &committee_index in &committee_indexes {
            let key = RaiCommitteeVoteKey {
                voter: vote.voter,
                committee_index,
            };
            if let Some(previous) = self.vote_states.get(&key) {
                previous.ensure_can_apply(vote)?;
            }
        }

        for committee_index in committee_indexes {
            let key = RaiCommitteeVoteKey {
                voter: vote.voter,
                committee_index,
            };
            self.vote_states.entry(key).or_default().apply_vote(vote);
        }

        self.update_tallies(committees);
        Ok(())
    }

    fn accepts_vote(&self, vote: &RaiVote) -> bool {
        if vote.kind == RaiVoteKind::Final && vote.value == RaiElectionValue::Timeout {
            return false;
        }

        match (&self.id, &vote.value) {
            (RaiElectionId::Slot { .. }, RaiElectionValue::Block(_))
            | (RaiElectionId::Slot { .. }, RaiElectionValue::Timeout)
            | (
                RaiElectionId::CloseCut { .. },
                RaiElectionValue::CloseCutHash(_) | RaiElectionValue::Timeout,
            )
            | (
                RaiElectionId::CloseRecord { .. },
                RaiElectionValue::CloseRecordHash(_) | RaiElectionValue::Timeout,
            ) => !matches!(
                (&self.id, vote.kind),
                (
                    RaiElectionId::CloseCut { .. } | RaiElectionId::CloseRecord { .. },
                    RaiVoteKind::Final
                )
            ),
            _ => false,
        }
    }

    fn update_tallies(&mut self, committees: &RaiCommitteeSet) {
        self.recalculate_tallies(committees);

        let merged_outcome = self.merged_outcome(committees);
        if let Some(outcome) = &merged_outcome
            && self.is_terminal_outcome(outcome)
        {
            self.status = RaiElectionStatus::Confirmed;
            if self.confirmed_value.is_none() {
                self.confirmed_value = self.terminal_confirmed_value(outcome);
            }
        }

        self.winner = self
            .confirmed_value
            .clone()
            .or_else(|| merged_outcome.as_ref().and_then(RaiElectionOutcome::value))
            .or_else(|| winner(&self.tallies).map(|(value, _)| value))
            .or_else(|| winner(&self.notarization_tallies).map(|(value, _)| value));
    }

    fn is_terminal_outcome(&self, outcome: &RaiElectionOutcome) -> bool {
        match (&self.id, outcome) {
            (_, RaiElectionOutcome::SafetyFault) => false,
            (RaiElectionId::Slot { .. }, _) => true,
            (
                RaiElectionId::CloseCut { .. } | RaiElectionId::CloseRecord { .. },
                RaiElectionOutcome::Timeout
                | RaiElectionOutcome::Notarized(_)
                | RaiElectionOutcome::Fast(_),
            ) => true,
            (
                RaiElectionId::CloseCut { .. } | RaiElectionId::CloseRecord { .. },
                RaiElectionOutcome::Final(_),
            ) => false,
        }
    }

    fn terminal_confirmed_value(&self, outcome: &RaiElectionOutcome) -> Option<RaiElectionValue> {
        match (&self.id, outcome) {
            (_, RaiElectionOutcome::Timeout) => Some(RaiElectionValue::Timeout),
            (RaiElectionId::Slot { .. }, RaiElectionOutcome::Notarized(value))
            | (RaiElectionId::Slot { .. }, RaiElectionOutcome::Fast(value))
            | (RaiElectionId::Slot { .. }, RaiElectionOutcome::Final(value))
            | (
                RaiElectionId::CloseCut { .. } | RaiElectionId::CloseRecord { .. },
                RaiElectionOutcome::Fast(value),
            ) => Some(value.clone()),
            _ => None,
        }
    }

    fn recalculate_tallies(&mut self, committees: &RaiCommitteeSet) {
        self.tallies.clear();
        self.notarization_tallies.clear();
        self.final_tallies.clear();

        for (key, state) in &self.vote_states {
            if key.committee_index >= committees.len() {
                continue;
            }

            if let Some(first) = &state.first {
                let tallies = self
                    .tallies
                    .entry(first.clone())
                    .or_insert_with(|| RaiCommitteeVoteCounts::new(committees.len()));
                tallies.add_vote(key.committee_index);
            }

            for value in state.notarization_values() {
                let notarization_tallies = self
                    .notarization_tallies
                    .entry(value)
                    .or_insert_with(|| RaiCommitteeVoteCounts::new(committees.len()));
                notarization_tallies.add_vote(key.committee_index);
            }

            if let Some(final_vote) = &state.final_vote {
                let final_tallies = self
                    .final_tallies
                    .entry(final_vote.clone())
                    .or_insert_with(|| RaiCommitteeVoteCounts::new(committees.len()));
                final_tallies.add_vote(key.committee_index);
            }
        }
    }

    fn local_outcome(
        &self,
        committee_index: usize,
        committee: &RaiCommittee,
    ) -> Option<RaiElectionOutcome> {
        let final_values = certified_values(
            &self.final_tallies,
            committee_index,
            |votes| committee.has_final_quorum(votes),
            false,
        );
        if final_values.len() > 1 {
            return Some(RaiElectionOutcome::SafetyFault);
        }

        let fast_values = certified_values(
            &self.tallies,
            committee_index,
            |votes| committee.has_fast_quorum(votes),
            false,
        );
        let notarized_values = certified_values(
            &self.notarization_tallies,
            committee_index,
            |votes| committee.has_notarization_quorum(votes),
            false,
        );
        let timeout_certified = self
            .notarization_tallies
            .get(&RaiElectionValue::Timeout)
            .is_some_and(|counts| {
                committee.has_notarization_quorum(counts.count_for(committee_index))
            });

        if let Some(final_value) = final_values.first() {
            let conflicting_notarization =
                timeout_certified || notarized_values.iter().any(|value| value != final_value);
            if conflicting_notarization {
                return Some(RaiElectionOutcome::SafetyFault);
            }
            return Some(RaiElectionOutcome::Final(final_value.clone()));
        }

        if timeout_certified && !fast_values.is_empty() {
            return Some(RaiElectionOutcome::SafetyFault);
        }

        if timeout_certified {
            return Some(RaiElectionOutcome::Timeout);
        }

        if fast_values.len() > 1 {
            return Some(RaiElectionOutcome::Timeout);
        }

        if let Some(fast_value) = fast_values.first() {
            if notarized_values.iter().any(|value| value != fast_value) {
                return Some(RaiElectionOutcome::SafetyFault);
            }
            return Some(RaiElectionOutcome::Fast(fast_value.clone()));
        }

        match notarized_values.as_slice() {
            [] => None,
            [value] => Some(RaiElectionOutcome::Notarized(value.clone())),
            _ => Some(RaiElectionOutcome::Timeout),
        }
    }

    fn voter_can_timeout(&self, voter: PublicKey, committee_index: usize) -> bool {
        let key = RaiCommitteeVoteKey {
            voter,
            committee_index,
        };
        self.vote_states
            .get(&key)
            .is_some_and(RaiVoteState::can_timeout_vote)
    }

    fn timeout_ready(&self, committee_index: usize, committee: &RaiCommittee) -> bool {
        if committee.is_empty() {
            return false;
        }

        let mut all_votes = 0usize;
        let mut first_counts = HashMap::<RaiElectionValue, usize>::new();
        for (key, state) in &self.vote_states {
            if key.committee_index != committee_index {
                continue;
            }

            let Some(first) = &state.first else {
                continue;
            };

            all_votes += 1;
            if first != &RaiElectionValue::Timeout {
                *first_counts.entry(first.clone()).or_default() += 1;
            }
        }

        let max_votes = first_counts.values().copied().max().unwrap_or_default();
        let threshold = committee.thresholds().max_faulty + committee.thresholds().max_offline + 1;
        all_votes.saturating_sub(max_votes) >= threshold
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RaiCommitteeVoteKey {
    pub voter: PublicKey,
    pub committee_index: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RaiVoteState {
    first: Option<RaiElectionValue>,
    notarized: Vec<RaiElectionValue>,
    final_vote: Option<RaiElectionValue>,
}

impl RaiVoteState {
    fn from_snapshot(snapshot: RaiVoteStateSnapshot) -> Self {
        Self {
            first: snapshot.first,
            notarized: sorted_election_values(snapshot.notarized),
            final_vote: snapshot.final_vote,
        }
    }

    fn snapshot(&self, key: RaiCommitteeVoteKey) -> RaiVoteStateSnapshot {
        RaiVoteStateSnapshot {
            voter: key.voter,
            committee_index: key.committee_index,
            first: self.first.clone(),
            notarized: sorted_election_values(self.notarized.clone()),
            final_vote: self.final_vote.clone(),
        }
    }

    fn ensure_can_apply(&self, vote: &RaiVote) -> Result<(), VoteError> {
        match vote.kind {
            RaiVoteKind::First => {
                if let Some(first) = &self.first {
                    if first == &vote.value {
                        Err(VoteError::Replay)
                    } else {
                        Err(VoteError::Invalid)
                    }
                } else if self.final_vote.is_some()
                    || self
                        .notarized
                        .iter()
                        .any(|notarized| notarized != &vote.value)
                {
                    Err(VoteError::Invalid)
                } else {
                    Ok(())
                }
            }
            RaiVoteKind::Notarization => {
                if self.notarized.contains(&vote.value) {
                    Err(VoteError::Replay)
                } else if self.final_vote.is_some() {
                    Err(VoteError::Invalid)
                } else {
                    Ok(())
                }
            }
            RaiVoteKind::Final => {
                if self.final_vote.is_some() {
                    Err(VoteError::Replay)
                } else if self.has_support_conflicting_with(&vote.value) {
                    Err(VoteError::Invalid)
                } else {
                    Ok(())
                }
            }
        }
    }

    fn apply_vote(&mut self, vote: &RaiVote) {
        match vote.kind {
            RaiVoteKind::First => self.first = Some(vote.value.clone()),
            RaiVoteKind::Notarization => {
                self.notarized.push(vote.value.clone());
                self.notarized = sorted_election_values(std::mem::take(&mut self.notarized));
            }
            RaiVoteKind::Final => self.final_vote = Some(vote.value.clone()),
        }
    }

    fn notarization_values(&self) -> Vec<RaiElectionValue> {
        let mut values = self.notarized.clone();
        if let Some(first) = &self.first
            && *first != RaiElectionValue::Timeout
            && !values.contains(first)
        {
            values.push(first.clone());
        }
        sorted_election_values(values)
    }

    fn has_support_conflicting_with(&self, value: &RaiElectionValue) -> bool {
        self.first.as_ref().is_some_and(|first| first != value)
            || self.notarized.iter().any(|notarized| notarized != value)
    }

    fn can_timeout_vote(&self) -> bool {
        self.first.is_some()
            && self.final_vote.is_none()
            && !self.notarized.contains(&RaiElectionValue::Timeout)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiVoteStateSnapshot {
    pub voter: PublicKey,
    pub committee_index: usize,
    pub first: Option<RaiElectionValue>,
    pub notarized: Vec<RaiElectionValue>,
    pub final_vote: Option<RaiElectionValue>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RaiCommitteeVoteCounts {
    per_committee: Vec<usize>,
}

impl RaiCommitteeVoteCounts {
    fn new(committee_count: usize) -> Self {
        Self {
            per_committee: vec![0; committee_count],
        }
    }

    fn from_snapshot(per_committee: Vec<usize>) -> Self {
        Self { per_committee }
    }

    fn add_vote(&mut self, committee_index: usize) {
        if let Some(count) = self.per_committee.get_mut(committee_index) {
            *count += 1;
        }
    }

    fn total(&self) -> usize {
        self.per_committee.iter().sum()
    }

    fn count_for(&self, committee_index: usize) -> usize {
        self.per_committee
            .get(committee_index)
            .copied()
            .unwrap_or_default()
    }
}

fn winner(
    tallies: &HashMap<RaiElectionValue, RaiCommitteeVoteCounts>,
) -> Option<(RaiElectionValue, usize)> {
    tallies
        .iter()
        .max_by_key(|(_, tally)| tally.total())
        .map(|(value, tally)| (value.clone(), tally.total()))
}

fn certified_values(
    tallies: &HashMap<RaiElectionValue, RaiCommitteeVoteCounts>,
    committee_index: usize,
    threshold: impl Fn(usize) -> bool,
    include_timeout: bool,
) -> Vec<RaiElectionValue> {
    sorted_election_values(
        tallies
            .iter()
            .filter(|(value, counts)| {
                (include_timeout || **value != RaiElectionValue::Timeout)
                    && threshold(counts.count_for(committee_index))
            })
            .map(|(value, _)| value.clone()),
    )
}

fn merge_outcomes(left: RaiElectionOutcome, right: RaiElectionOutcome) -> RaiElectionOutcome {
    use RaiElectionOutcome::*;

    match (left, right) {
        (SafetyFault, _) | (_, SafetyFault) => SafetyFault,
        (Timeout, _) | (_, Timeout) => Timeout,
        (Final(left), Final(right)) if left != right => SafetyFault,
        (Notarized(left), Notarized(right))
        | (Notarized(left), Fast(right))
        | (Fast(left), Notarized(right))
        | (Notarized(left), Final(right))
        | (Final(left), Notarized(right))
        | (Fast(left), Fast(right))
        | (Fast(left), Final(right))
        | (Final(left), Fast(right))
        | (Final(left), Final(right))
            if left != right =>
        {
            Timeout
        }
        (Notarized(value), Notarized(_))
        | (Notarized(value), Fast(_))
        | (Fast(value), Notarized(_))
        | (Notarized(value), Final(_))
        | (Final(value), Notarized(_)) => Notarized(value),
        (Fast(value), Fast(_)) => Fast(value),
        (Fast(value), Final(_)) | (Final(value), Fast(_)) | (Final(value), Final(_)) => {
            Final(value)
        }
    }
}

impl RaiElectionOutcome {
    fn value(&self) -> Option<RaiElectionValue> {
        match self {
            Self::Timeout => Some(RaiElectionValue::Timeout),
            Self::Notarized(value) | Self::Fast(value) | Self::Final(value) => Some(value.clone()),
            Self::SafetyFault => None,
        }
    }
}

fn snapshot_tallies(
    tallies: &HashMap<RaiElectionValue, RaiCommitteeVoteCounts>,
) -> Vec<RaiTallySnapshot> {
    let mut snapshots: Vec<_> = tallies
        .iter()
        .map(|(value, counts)| RaiTallySnapshot {
            value: value.clone(),
            per_committee: counts.per_committee.clone(),
        })
        .collect();
    snapshots.sort_by_key(|tally| serialized_election_value(&tally.value));
    snapshots
}

fn sorted_election_values(
    values: impl IntoIterator<Item = RaiElectionValue>,
) -> Vec<RaiElectionValue> {
    let mut result = Vec::new();
    for value in values {
        if !result.contains(&value) {
            result.push(value);
        }
    }
    result.sort_by_key(serialized_election_value);
    result
}

fn serialized_election_id(id: &RaiElectionId) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(RaiElectionId::SERIALIZED_SIZE);
    id.serialize(&mut bytes)
        .expect("writing to Vec should succeed");
    bytes
}

fn serialized_election_value(value: &RaiElectionValue) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(RaiElectionValue::SERIALIZED_SIZE);
    value
        .serialize(&mut bytes)
        .expect("writing to Vec should succeed");
    bytes
}

#[cfg(test)]
mod tests {
    use super::super::RaiCommitteeDeriver;
    use super::*;
    use rsnano_types::{Account, Amount, BlockHash, PrivateKey, RaiSlot};

    #[test]
    fn routes_vote_by_election_id() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let other_election_id = slot_election(2);
        let value = RaiElectionValue::Block(BlockHash::from(3));

        elections.insert(election_id.clone()).unwrap();

        let key = PrivateKey::from(1);
        let vote = RaiVote::new_first(&key, other_election_id.clone(), value.clone());
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        let result = elections.apply_vote(&vote, &committees);

        assert_eq!(result, Err(VoteError::Indeterminate));
        assert!(elections.election(&other_election_id).is_none());
        assert_eq!(elections.election(&election_id).unwrap().tally(&value), 0);
    }

    #[test]
    fn applies_vote_to_matching_election() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let vote = RaiVote::new_first(&key, election_id.clone(), value.clone());

        assert_eq!(elections.apply_vote(&vote, &committees), Ok(()));

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 1);
        assert_eq!(election.winner(), Some(&value));
    }

    #[test]
    fn discards_epoch_slots_outside_cut() {
        let elections = RaiActiveElections::new();
        let included_slot = RaiSlot::new(Account::from(1), 1);
        let excluded_slot = RaiSlot::new(Account::from(2), 1);
        let included = RaiElectionId::Slot {
            slot: included_slot,
            epoch: 1,
        };
        let excluded = RaiElectionId::Slot {
            slot: excluded_slot,
            epoch: 1,
        };
        let previous_epoch = RaiElectionId::Slot {
            slot: excluded_slot,
            epoch: 0,
        };
        let close_cut = RaiElectionId::CloseCut {
            epoch: 1,
            attempt: 0,
        };
        let cut = [included_slot].into_iter().collect();

        for election_id in [
            included.clone(),
            excluded.clone(),
            previous_epoch.clone(),
            close_cut.clone(),
        ] {
            elections.insert(election_id).unwrap();
        }

        assert!(elections.discard_slots_outside_cut(1, &cut));
        assert!(elections.contains(&included));
        assert!(!elections.contains(&excluded));
        assert!(elections.contains(&previous_epoch));
        assert!(elections.contains(&close_cut));
        assert!(!elections.discard_slots_outside_cut(1, &cut));
    }

    #[test]
    fn final_vote_confirms_election_when_it_reaches_quorum() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let vote = RaiVote::new_final(&key, election_id.clone(), value.clone());

        assert_eq!(elections.apply_vote(&vote, &committees), Ok(()));

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.status(), RaiElectionStatus::Confirmed);
        assert_eq!(election.confirmed_value(), Some(&value));
        assert_eq!(election.final_tally(&value), 1);
    }

    #[test]
    fn notarization_certificate_finishes_slot_election() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_notarization(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(
            election.merged_outcome(&committees),
            Some(RaiElectionOutcome::Notarized(value.clone()))
        );
        assert_eq!(election.status(), RaiElectionStatus::Confirmed);
        assert_eq!(election.confirmed_value(), Some(&value));
    }

    #[test]
    fn timeout_notarization_certificate_finishes_as_timeout() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_notarization(&key, election_id.clone(), RaiElectionValue::Timeout),
                &committees
            ),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(
            election.merged_outcome(&committees),
            Some(RaiElectionOutcome::Timeout)
        );
        assert_eq!(election.status(), RaiElectionStatus::Confirmed);
        assert_eq!(election.confirmed_value(), Some(&RaiElectionValue::Timeout));
    }

    #[test]
    fn same_value_fast_and_final_local_results_merge_to_final() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let first_key = PrivateKey::from(1);
        let second_key = PrivateKey::from(2);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let committees = RaiCommitteeSet::new([
            raw_committee([(first_key.public_key(), Amount::raw(100))]),
            raw_committee([(second_key.public_key(), Amount::raw(100))]),
        ]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&first_key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_final(&second_key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(
            election.merged_outcome(&committees),
            Some(RaiElectionOutcome::Final(value.clone()))
        );
        assert_eq!(election.confirmed_value(), Some(&value));
    }

    #[test]
    fn conflicting_non_timeout_local_results_merge_to_timeout() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let first_key = PrivateKey::from(1);
        let second_key = PrivateKey::from(2);
        let first_value = RaiElectionValue::Block(BlockHash::from(3));
        let second_value = RaiElectionValue::Block(BlockHash::from(4));
        let committees = RaiCommitteeSet::new([
            raw_committee([(first_key.public_key(), Amount::raw(100))]),
            raw_committee([(second_key.public_key(), Amount::raw(100))]),
        ]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&first_key, election_id.clone(), first_value),
                &committees
            ),
            Ok(())
        );
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&second_key, election_id.clone(), second_value),
                &committees
            ),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(
            election.merged_outcome(&committees),
            Some(RaiElectionOutcome::Timeout)
        );
        assert_eq!(election.confirmed_value(), Some(&RaiElectionValue::Timeout));
    }

    #[test]
    fn non_timeout_close_notarization_converges_without_certifying_value() {
        let elections = RaiActiveElections::new();
        let election_id = RaiElectionId::CloseCut {
            epoch: 1,
            attempt: 0,
        };
        let value = RaiElectionValue::CloseCutHash(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_notarization(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(
            election.merged_outcome(&committees),
            Some(RaiElectionOutcome::Notarized(value))
        );
        assert_eq!(election.status(), RaiElectionStatus::Confirmed);
        assert_eq!(election.confirmed_value(), None);
    }

    #[test]
    fn rejects_values_that_do_not_belong_to_the_election_kind() {
        let elections = RaiActiveElections::new();
        let election_id = RaiElectionId::CloseCut {
            epoch: 1,
            attempt: 0,
        };
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let vote = RaiVote::new_first(
            &key,
            election_id,
            RaiElectionValue::CloseRecordHash(BlockHash::from(3)),
        );

        assert_eq!(
            elections.apply_vote(&vote, &committees),
            Err(VoteError::Invalid)
        );
    }

    #[test]
    fn rejects_final_timeout_vote() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let vote = RaiVote::new_final(&key, election_id, RaiElectionValue::Timeout);

        assert_eq!(
            elections.apply_vote(&vote, &committees),
            Err(VoteError::Invalid)
        );
    }

    #[test]
    fn rejects_final_vote_for_close_election() {
        let elections = RaiActiveElections::new();
        let election_id = RaiElectionId::CloseCut {
            epoch: 1,
            attempt: 0,
        };
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let vote = RaiVote::new_final(
            &key,
            election_id,
            RaiElectionValue::CloseCutHash(BlockHash::from(3)),
        );

        assert_eq!(
            elections.apply_vote(&vote, &committees),
            Err(VoteError::Invalid)
        );
    }

    #[test]
    fn duplicate_same_kind_vote_from_same_rep_is_replay() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let first = RaiVote::new_first(&key, election_id.clone(), value.clone());
        let second = RaiVote::new_first(&key, election_id, value);

        assert_eq!(elections.apply_vote(&first, &committees), Ok(()));
        assert_eq!(
            elections.apply_vote(&second, &committees),
            Err(VoteError::Replay)
        );
    }

    #[test]
    fn conflicting_first_vote_from_same_rep_is_invalid() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let first_value = RaiElectionValue::Block(BlockHash::from(3));
        let conflicting_value = RaiElectionValue::Block(BlockHash::from(4));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), first_value.clone()),
                &committees
            ),
            Ok(())
        );
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), conflicting_value.clone()),
                &committees
            ),
            Err(VoteError::Invalid)
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&first_value), 1);
        assert_eq!(election.tally(&conflicting_value), 0);
        assert_eq!(
            election.vote_state_snapshots(),
            vec![RaiVoteStateSnapshot {
                voter: key.public_key(),
                committee_index: 0,
                first: Some(first_value),
                notarized: Vec::new(),
                final_vote: None,
            }]
        );
    }

    #[test]
    fn conflicting_first_vote_after_notarization_is_invalid() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let notarized_value = RaiElectionValue::Block(BlockHash::from(3));
        let conflicting_value = RaiElectionValue::Block(BlockHash::from(4));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_notarization(&key, election_id.clone(), notarized_value.clone()),
                &committees
            ),
            Ok(())
        );
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), conflicting_value.clone()),
                &committees
            ),
            Err(VoteError::Invalid)
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&conflicting_value), 0);
    }

    #[test]
    fn first_vote_can_arrive_after_notarization_without_erasing_it() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_notarization(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(
            election.vote_state_snapshots(),
            vec![RaiVoteStateSnapshot {
                voter: key.public_key(),
                committee_index: 0,
                first: Some(value.clone()),
                notarized: vec![value.clone()],
                final_vote: None,
            }]
        );
        assert_eq!(election.tally(&value), 1);
    }

    #[test]
    fn final_vote_does_not_overwrite_first_vote() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_final(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 1);
        assert_eq!(election.final_tally(&value), 1);
        assert_eq!(
            election.vote_state_snapshots(),
            vec![RaiVoteStateSnapshot {
                voter: key.public_key(),
                committee_index: 0,
                first: Some(value.clone()),
                notarized: Vec::new(),
                final_vote: Some(value),
            }]
        );
    }

    #[test]
    fn final_vote_rejects_conflicting_prior_support() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let first_value = RaiElectionValue::Block(BlockHash::from(3));
        let final_value = RaiElectionValue::Block(BlockHash::from(4));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), first_value.clone()),
                &committees
            ),
            Ok(())
        );
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_final(&key, election_id.clone(), final_value.clone()),
                &committees
            ),
            Err(VoteError::Invalid)
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&first_value), 1);
        assert_eq!(election.final_tally(&final_value), 0);
        assert_eq!(
            election.vote_state_snapshots(),
            vec![RaiVoteStateSnapshot {
                voter: key.public_key(),
                committee_index: 0,
                first: Some(first_value),
                notarized: Vec::new(),
                final_vote: None,
            }]
        );
    }

    #[test]
    fn final_vote_locks_out_later_first_or_notarization_votes() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_final(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Err(VoteError::Invalid)
        );
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_notarization(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Err(VoteError::Invalid)
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 0);
        assert_eq!(election.final_tally(&value), 1);
    }

    #[test]
    fn unscoped_vote_creates_independent_state_for_each_member_committee() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let key = PrivateKey::from(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let first_committee = raw_committee([
            (key.public_key(), Amount::raw(100)),
            (PublicKey::from(2), Amount::raw(100)),
        ]);
        let second_committee = raw_committee([
            (key.public_key(), Amount::raw(100)),
            (PublicKey::from(3), Amount::raw(100)),
        ]);
        let committees = RaiCommitteeSet::new([first_committee, second_committee]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(
            election.vote_state_snapshots(),
            vec![
                RaiVoteStateSnapshot {
                    voter: key.public_key(),
                    committee_index: 0,
                    first: Some(value.clone()),
                    notarized: Vec::new(),
                    final_vote: None,
                },
                RaiVoteStateSnapshot {
                    voter: key.public_key(),
                    committee_index: 1,
                    first: Some(value),
                    notarized: Vec::new(),
                    final_vote: None,
                },
            ]
        );
    }

    #[test]
    fn scoped_vote_applies_only_to_named_committee() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let key = PrivateKey::from(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let first_committee = raw_committee([
            (key.public_key(), Amount::raw(100)),
            (PublicKey::from(2), Amount::raw(100)),
        ]);
        let second_committee = raw_committee([
            (key.public_key(), Amount::raw(100)),
            (PublicKey::from(3), Amount::raw(100)),
        ]);
        let committees = RaiCommitteeSet::new([first_committee, second_committee]);

        elections.insert(election_id.clone()).unwrap();
        assert_eq!(
            elections.apply_vote(
                &RaiVote::new_first_scoped(&key, 1, election_id.clone(), value.clone()),
                &committees
            ),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(
            election.vote_state_snapshots(),
            vec![RaiVoteStateSnapshot {
                voter: key.public_key(),
                committee_index: 1,
                first: Some(value.clone()),
                notarized: Vec::new(),
                final_vote: None,
            }]
        );
        assert_eq!(
            election.merged_outcome(&committees),
            None,
            "committee 0 has no local outcome"
        );
    }

    #[test]
    fn returns_unfinished_slots_for_epoch() {
        let elections = RaiActiveElections::new();
        let active = slot_election(1);
        let confirmed = slot_election(2);
        let other_epoch = RaiElectionId::Slot {
            slot: RaiSlot::new(Account::from(1), 3),
            epoch: 2,
        };
        let key = PrivateKey::from(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(active.clone()).unwrap();
        elections.insert(confirmed.clone()).unwrap();
        elections.insert(other_epoch).unwrap();
        elections
            .apply_vote(&RaiVote::new_final(&key, confirmed, value), &committees)
            .unwrap();

        assert_eq!(
            elections.unfinished_slots(1),
            vec![RaiSlot::new(Account::from(1), 1)]
        );
    }

    #[test]
    fn exposes_fast_value_when_each_committee_has_fast_quorum() {
        let elections = RaiActiveElections::new();
        let election_id = RaiElectionId::CloseCut {
            epoch: 1,
            attempt: 0,
        };
        let key = PrivateKey::from(1);
        let value = RaiElectionValue::CloseCutHash(BlockHash::from(3));
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        elections
            .apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), value.clone()),
                &committees,
            )
            .unwrap();

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.fast_value(&committees), Some(value.clone()));
        assert_eq!(election.confirmed_value(), Some(&value));
    }

    #[test]
    fn timeout_is_not_ready_after_single_member_fast_vote() {
        let elections = RaiActiveElections::new();
        let election_id = RaiElectionId::CloseCut {
            epoch: 1,
            attempt: 0,
        };
        let key = PrivateKey::from(1);
        let value = RaiElectionValue::CloseCutHash(BlockHash::from(3));
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        elections
            .apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), value),
                &committees,
            )
            .unwrap();

        let election = elections.election(&election_id).unwrap();
        assert!(
            election
                .timeout_ready_committee_indexes(&key.public_key(), &committees)
                .is_empty()
        );
    }

    #[test]
    fn timeout_is_ready_when_split_first_votes_block_fast_quorum() {
        let elections = RaiActiveElections::new();
        let election_id = RaiElectionId::CloseCut {
            epoch: 1,
            attempt: 0,
        };
        let keys: Vec<_> = (1..=5).map(PrivateKey::from).collect();
        let first_value = RaiElectionValue::CloseCutHash(BlockHash::from(3));
        let second_value = RaiElectionValue::CloseCutHash(BlockHash::from(4));
        let committees = committee([
            (keys[0].public_key(), Amount::raw(100)),
            (keys[1].public_key(), Amount::raw(100)),
            (keys[2].public_key(), Amount::raw(100)),
            (keys[3].public_key(), Amount::raw(100)),
            (keys[4].public_key(), Amount::raw(100)),
        ]);

        elections.insert(election_id.clone()).unwrap();
        for key in keys.iter().take(3) {
            elections
                .apply_vote(
                    &RaiVote::new_first(key, election_id.clone(), first_value.clone()),
                    &committees,
                )
                .unwrap();
        }
        for key in keys.iter().skip(3) {
            elections
                .apply_vote(
                    &RaiVote::new_first(key, election_id.clone(), second_value.clone()),
                    &committees,
                )
                .unwrap();
        }

        let election = elections.election(&election_id).unwrap();
        assert_eq!(
            election.timeout_ready_committee_indexes(&keys[0].public_key(), &committees),
            vec![0]
        );
    }

    fn slot_election(account_height: u64) -> RaiElectionId {
        RaiElectionId::Slot {
            slot: RaiSlot::new(Account::from(1), account_height),
            epoch: 1,
        }
    }

    fn committee<const N: usize>(values: [(PublicKey, Amount); N]) -> RaiCommitteeSet {
        RaiCommitteeSet::single(raw_committee(values))
    }

    fn raw_committee<const N: usize>(
        values: [(PublicKey, Amount); N],
    ) -> super::super::RaiCommittee {
        RaiCommitteeDeriver::new().derive_committee(values)
    }
}
