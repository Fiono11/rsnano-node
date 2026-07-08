use std::{collections::HashMap, mem::size_of, sync::RwLock};

use rsnano_ledger::RepWeights;
use rsnano_types::{
    Amount, PublicKey, RaiElectionId, RaiElectionValue, RaiVote, RaiVoteKind, VoteError,
};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

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

    pub fn apply_vote(
        &self,
        vote: &RaiVote,
        rep_weights: &RepWeights,
        quorum_delta: Amount,
    ) -> Result<(), VoteError> {
        let mut guard = self.data.write().unwrap();
        let Some(election) = guard.elections.get_mut(&vote.election_id) else {
            return Err(VoteError::Indeterminate);
        };

        election.apply_vote(vote, rep_weights, quorum_delta)
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
    tallies: HashMap<RaiElectionValue, Amount>,
    final_tallies: HashMap<RaiElectionValue, Amount>,
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

    pub fn tally(&self, value: &RaiElectionValue) -> Amount {
        self.tallies.get(value).cloned().unwrap_or_default()
    }

    pub fn final_tally(&self, value: &RaiElectionValue) -> Amount {
        self.final_tallies.get(value).cloned().unwrap_or_default()
    }

    pub fn winner(&self) -> Option<&RaiElectionValue> {
        self.winner.as_ref()
    }

    pub fn confirmed_value(&self) -> Option<&RaiElectionValue> {
        self.confirmed_value.as_ref()
    }

    fn apply_vote(
        &mut self,
        vote: &RaiVote,
        rep_weights: &RepWeights,
        quorum_delta: Amount,
    ) -> Result<(), VoteError> {
        if self.status == RaiElectionStatus::Confirmed {
            return Err(VoteError::Late);
        }

        if !self.accepts_value(&vote.value) {
            return Err(VoteError::Invalid);
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
                weight: Amount::ZERO,
            },
        );

        self.update_tallies(rep_weights, quorum_delta);
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

    fn update_tallies(&mut self, rep_weights: &RepWeights, quorum_delta: Amount) {
        self.update_vote_weights(rep_weights);
        self.recalculate_tallies();
        self.winner = winner(&self.tallies).map(|(value, _)| value);

        if let Some((value, tally)) = winner(&self.final_tallies)
            && !tally.is_zero()
            && tally >= quorum_delta
        {
            self.status = RaiElectionStatus::Confirmed;
            self.confirmed_value = Some(value);
        }
    }

    fn update_vote_weights(&mut self, rep_weights: &RepWeights) {
        for vote in self.votes.values_mut() {
            vote.weight = rep_weights.weight(&vote.voter);
        }
    }

    fn recalculate_tallies(&mut self) {
        self.tallies.clear();
        self.final_tallies.clear();

        for vote in self.votes.values() {
            *self.tallies.entry(vote.value.clone()).or_default() += vote.weight;

            if vote.kind == RaiVoteKind::Final {
                *self.final_tallies.entry(vote.value.clone()).or_default() += vote.weight;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiVoteSummary {
    pub voter: PublicKey,
    pub kind: RaiVoteKind,
    pub value: RaiElectionValue,
    pub weight: Amount,
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

fn winner(tallies: &HashMap<RaiElectionValue, Amount>) -> Option<(RaiElectionValue, Amount)> {
    tallies
        .iter()
        .max_by_key(|(_, tally)| *tally)
        .map(|(value, tally)| (value.clone(), *tally))
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
    use super::*;
    use rsnano_ledger::RepWeights;
    use rsnano_types::{Account, BlockHash, PrivateKey, RaiSlot};

    #[test]
    fn routes_vote_by_election_id() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let other_election_id = slot_election(2);
        let value = RaiElectionValue::Block(BlockHash::from(3));

        elections.insert(election_id.clone()).unwrap();

        let key = PrivateKey::from(1);
        let vote = RaiVote::new_first(&key, other_election_id.clone(), value.clone());
        let rep_weights = rep_weights([(key.public_key(), Amount::raw(100))]);

        let result = elections.apply_vote(&vote, &rep_weights, Amount::raw(67));

        assert_eq!(result, Err(VoteError::Indeterminate));
        assert!(elections.election(&other_election_id).is_none());
        assert_eq!(
            elections.election(&election_id).unwrap().tally(&value),
            Amount::ZERO
        );
    }

    #[test]
    fn applies_vote_to_matching_election() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let rep_weights = rep_weights([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let vote = RaiVote::new_first(&key, election_id.clone(), value.clone());

        assert_eq!(
            elections.apply_vote(&vote, &rep_weights, Amount::raw(67)),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), Amount::raw(100));
        assert_eq!(election.winner(), Some(&value));
    }

    #[test]
    fn final_vote_confirms_election_when_it_reaches_quorum() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let rep_weights = rep_weights([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let vote = RaiVote::new_final(&key, election_id.clone(), value.clone());

        assert_eq!(
            elections.apply_vote(&vote, &rep_weights, Amount::raw(67)),
            Ok(())
        );

        let election = elections.election(&election_id).unwrap();
        assert_eq!(election.status(), RaiElectionStatus::Confirmed);
        assert_eq!(election.confirmed_value(), Some(&value));
        assert_eq!(election.final_tally(&value), Amount::raw(100));
    }

    #[test]
    fn rejects_values_that_do_not_belong_to_the_election_kind() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let key = PrivateKey::from(1);
        let rep_weights = rep_weights([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let vote = RaiVote::new_first(&key, election_id, RaiElectionValue::Timeout);

        assert_eq!(
            elections.apply_vote(&vote, &rep_weights, Amount::raw(67)),
            Err(VoteError::Invalid)
        );
    }

    #[test]
    fn lower_or_same_kind_vote_from_same_rep_is_replay() {
        let elections = RaiActiveElections::new();
        let election_id = slot_election(1);
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let key = PrivateKey::from(1);
        let rep_weights = rep_weights([(key.public_key(), Amount::raw(100))]);

        elections.insert(election_id.clone()).unwrap();
        let first = RaiVote::new_notarization(&key, election_id.clone(), value.clone());
        let second = RaiVote::new_first(&key, election_id, value);

        assert_eq!(
            elections.apply_vote(&first, &rep_weights, Amount::raw(67)),
            Ok(())
        );
        assert_eq!(
            elections.apply_vote(&second, &rep_weights, Amount::raw(67)),
            Err(VoteError::Replay)
        );
    }

    fn slot_election(account_height: u64) -> RaiElectionId {
        RaiElectionId::Slot {
            slot: RaiSlot::new(Account::from(1), account_height),
            epoch: 1,
        }
    }

    fn rep_weights<const N: usize>(values: [(PublicKey, Amount); N]) -> RepWeights {
        RepWeights::from(values)
    }
}
