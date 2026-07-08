use std::{collections::HashMap, mem::size_of, sync::RwLock};

use rsnano_types::{
    PublicKey, RaiElectionId, RaiElectionValue, RaiSlot, RaiVote, RaiVoteKind, VoteError,
};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

use super::RaiCommitteeSet;

pub struct RaiActiveElections {
    data: RwLock<RaiActiveElectionsData>,
}

impl RaiActiveElections {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(RaiActiveElectionsData::default()),
        }
    }

    pub fn insert(&self, election_id: RaiElectionId) -> Result<(), RaiElectionInsertError> {
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

    pub fn apply_vote(
        &self,
        vote: &RaiVote,
        committees: &RaiCommitteeSet,
    ) -> Result<(), VoteError> {
        let mut guard = self.data.write().unwrap();
        let Some(election) = guard.elections.get_mut(&vote.election_id) else {
            return Err(VoteError::Indeterminate);
        };

        election.apply_vote(vote, committees)
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
    votes: HashMap<PublicKey, RaiVoteSummary>,
    tallies: HashMap<RaiElectionValue, RaiCommitteeVoteCounts>,
    final_tallies: HashMap<RaiElectionValue, RaiCommitteeVoteCounts>,
    winner: Option<RaiElectionValue>,
    confirmed_value: Option<RaiElectionValue>,
}

impl RaiElection {
    pub fn new(id: RaiElectionId) -> Self {
        Self {
            id,
            status: RaiElectionStatus::Active,
            votes: HashMap::new(),
            tallies: HashMap::new(),
            final_tallies: HashMap::new(),
            winner: None,
            confirmed_value: None,
        }
    }

    pub fn id(&self) -> &RaiElectionId {
        &self.id
    }

    pub fn status(&self) -> RaiElectionStatus {
        self.status
    }

    pub fn votes(&self) -> &HashMap<PublicKey, RaiVoteSummary> {
        &self.votes
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
        self.tallies
            .iter()
            .find(|(_, counts)| counts.has_fast_quorum(committees))
            .map(|(value, _)| value.clone())
    }

    fn apply_vote(
        &mut self,
        vote: &RaiVote,
        committees: &RaiCommitteeSet,
    ) -> Result<(), VoteError> {
        if self.status == RaiElectionStatus::Confirmed {
            return Err(VoteError::Late);
        }

        if !self.accepts_value(&vote.value) {
            return Err(VoteError::Invalid);
        }

        let committee_votes = committees.committee_indexes_for(&vote.voter).len();
        if committee_votes == 0 {
            return Err(VoteError::Indeterminate);
        }

        if let Some(previous) = self.votes.get(&vote.voter) {
            previous.ensure_no_replay(vote)?;
        }

        self.votes.insert(
            vote.voter,
            RaiVoteSummary {
                voter: vote.voter,
                kind: vote.kind,
                value: vote.value.clone(),
                committee_votes: 0,
            },
        );

        self.update_tallies(committees);
        Ok(())
    }

    fn accepts_value(&self, value: &RaiElectionValue) -> bool {
        matches!(
            (&self.id, value),
            (RaiElectionId::Slot { .. }, RaiElectionValue::Block(_))
                | (RaiElectionId::Close { .. }, RaiElectionValue::CloseHash(_))
                | (RaiElectionId::Close { .. }, RaiElectionValue::Timeout)
        )
    }

    fn update_tallies(&mut self, committees: &RaiCommitteeSet) {
        self.update_vote_committee_counts(committees);
        self.recalculate_tallies(committees);
        self.winner = winner(&self.tallies).map(|(value, _)| value);

        if let Some((value, _)) = winner(&self.final_tallies)
            && self
                .final_tallies
                .get(&value)
                .is_some_and(|counts| counts.has_final_quorum(committees))
        {
            self.status = RaiElectionStatus::Confirmed;
            self.confirmed_value = Some(value);
        }
    }

    fn update_vote_committee_counts(&mut self, committees: &RaiCommitteeSet) {
        for vote in self.votes.values_mut() {
            vote.committee_votes = committees.committee_indexes_for(&vote.voter).len();
        }
    }

    fn recalculate_tallies(&mut self, committees: &RaiCommitteeSet) {
        self.tallies.clear();
        self.final_tallies.clear();

        for vote in self.votes.values() {
            let committee_indexes = committees.committee_indexes_for(&vote.voter);
            if committee_indexes.is_empty() {
                continue;
            }

            let tallies = self
                .tallies
                .entry(vote.value.clone())
                .or_insert_with(|| RaiCommitteeVoteCounts::new(committees.len()));
            tallies.add_votes(&committee_indexes);

            if vote.kind == RaiVoteKind::Final {
                let final_tallies = self
                    .final_tallies
                    .entry(vote.value.clone())
                    .or_insert_with(|| RaiCommitteeVoteCounts::new(committees.len()));
                final_tallies.add_votes(&committee_indexes);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiVoteSummary {
    pub voter: PublicKey,
    pub kind: RaiVoteKind,
    pub value: RaiElectionValue,
    pub committee_votes: usize,
}

impl RaiVoteSummary {
    fn ensure_no_replay(&self, new_vote: &RaiVote) -> Result<(), VoteError> {
        if vote_kind_rank(new_vote.kind) <= vote_kind_rank(self.kind) {
            Err(VoteError::Replay)
        } else {
            Ok(())
        }
    }
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

    fn add_votes(&mut self, committee_indexes: &[usize]) {
        for &committee_index in committee_indexes {
            if let Some(count) = self.per_committee.get_mut(committee_index) {
                *count += 1;
            }
        }
    }

    fn total(&self) -> usize {
        self.per_committee.iter().sum()
    }

    fn has_final_quorum(&self, committees: &RaiCommitteeSet) -> bool {
        committees
            .iter()
            .enumerate()
            .all(|(index, committee)| committee.has_final_quorum(self.count_for(index)))
    }

    fn has_fast_quorum(&self, committees: &RaiCommitteeSet) -> bool {
        committees
            .iter()
            .enumerate()
            .all(|(index, committee)| committee.has_fast_quorum(self.count_for(index)))
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

fn vote_kind_rank(kind: RaiVoteKind) -> u8 {
    match kind {
        RaiVoteKind::First => 0,
        RaiVoteKind::Notarization => 1,
        RaiVoteKind::Final => 2,
    }
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
    fn rejects_values_that_do_not_belong_to_the_election_kind() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let vote = RaiVote::new_first(&key, election_id, RaiElectionValue::Timeout);

        assert_eq!(
            elections.apply_vote(&vote, &committees),
            Err(VoteError::Invalid)
        );
    }

    #[test]
    fn lower_or_same_kind_vote_from_same_rep_is_replay() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let first = RaiVote::new_notarization(&key, election_id.clone(), value.clone());
        let second = RaiVote::new_first(&key, election_id, value);

        assert_eq!(elections.apply_vote(&first, &committees), Ok(()));
        assert_eq!(
            elections.apply_vote(&second, &committees),
            Err(VoteError::Replay)
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
        let election_id = RaiElectionId::Close {
            epoch: 1,
            attempt: 0,
        };
        let key = PrivateKey::from(1);
        let value = RaiElectionValue::CloseHash(BlockHash::from(3));
        let committees = committee([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        elections
            .apply_vote(
                &RaiVote::new_first(&key, election_id.clone(), value.clone()),
                &committees,
            )
            .unwrap();

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.fast_value(&committees), Some(value));
        assert_eq!(election.confirmed_value(), None);
    }

    fn slot_election(account_height: u64) -> RaiElectionId {
        RaiElectionId::Slot {
            slot: RaiSlot::new(Account::from(1), account_height),
            epoch: 1,
        }
    }

    fn committee<const N: usize>(values: [(PublicKey, Amount); N]) -> RaiCommitteeSet {
        RaiCommitteeSet::single(RaiCommitteeDeriver::new().derive_committee(values))
    }
}
