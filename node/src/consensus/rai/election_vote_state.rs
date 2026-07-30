use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use rsnano_ledger::RepWeights;
use rsnano_types::{Amount, BlockHash, PublicKey, RaiCommitteeScope};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockHashOrTimeout {
    Block(BlockHash),
    Timeout,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RaiOutcome {
    #[default]
    Pending,
    Confirmed(BlockHash),
    TimedOut,
}

#[derive(Clone, Debug, Default)]
pub struct RaiCommitteeVoteState {
    pub first: HashMap<PublicKey, BlockHashOrTimeout>,
    pub notar: HashMap<PublicKey, HashSet<BlockHashOrTimeout>>,
    pub final_votes: HashMap<PublicKey, BlockHash>,
    pub final_locked: HashSet<PublicKey>,
}

#[derive(Clone, Debug)]
pub struct RaiCommitteeInstance {
    pub weights: Arc<RepWeights>,
    pub votes: RaiCommitteeVoteState,
}

#[derive(Clone, Debug, Default)]
pub struct RaiElectionVoteState {
    pub committees: Vec<RaiCommitteeInstance>,
    pub outcome: RaiOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiVoteStateError {
    EmptyScope,
    SignerNotInCommittee,
    DuplicateFirstVote,
    FinalLocked,
    IncompatibleFinalSupport,
    TimeoutFinalVote,
}

impl RaiElectionVoteState {
    pub fn new(committees: Vec<Arc<RepWeights>>) -> Self {
        let mut unique = Vec::new();
        for weights in committees {
            if unique.iter().any(|existing: &RaiCommitteeInstance| {
                Arc::ptr_eq(&existing.weights, &weights) || existing.weights == weights
            }) {
                continue;
            }
            unique.push(RaiCommitteeInstance {
                weights,
                votes: RaiCommitteeVoteState::default(),
            });
        }

        Self {
            committees: unique,
            outcome: RaiOutcome::Pending,
        }
    }

    pub fn record_first_vote(
        &mut self,
        signer: PublicKey,
        value: BlockHashOrTimeout,
        scope: RaiCommitteeScope,
    ) -> Result<(), RaiVoteStateError> {
        let targets = self.checked_targets(signer, scope)?;
        if targets.iter().any(|&index| {
            let votes = &self.committees[index].votes;
            votes.final_locked.contains(&signer) || votes.first.contains_key(&signer)
        }) {
            return Err(
                if targets
                    .iter()
                    .any(|&index| self.committees[index].votes.final_locked.contains(&signer))
                {
                    RaiVoteStateError::FinalLocked
                } else {
                    RaiVoteStateError::DuplicateFirstVote
                },
            );
        }

        for index in targets {
            self.committees[index].votes.first.insert(signer, value);
        }
        Ok(())
    }

    pub fn record_notarization_vote(
        &mut self,
        signer: PublicKey,
        value: BlockHashOrTimeout,
        scope: RaiCommitteeScope,
    ) -> Result<(), RaiVoteStateError> {
        let targets = self.checked_targets(signer, scope)?;
        if targets
            .iter()
            .any(|&index| self.committees[index].votes.final_locked.contains(&signer))
        {
            return Err(RaiVoteStateError::FinalLocked);
        }

        for index in targets {
            self.committees[index]
                .votes
                .notar
                .entry(signer)
                .or_default()
                .insert(value);
        }
        Ok(())
    }

    pub fn record_final_vote(
        &mut self,
        signer: PublicKey,
        hash: BlockHash,
        scope: RaiCommitteeScope,
    ) -> Result<(), RaiVoteStateError> {
        self.record_final_value(signer, BlockHashOrTimeout::Block(hash), scope)
    }

    pub fn record_final_value(
        &mut self,
        signer: PublicKey,
        value: BlockHashOrTimeout,
        scope: RaiCommitteeScope,
    ) -> Result<(), RaiVoteStateError> {
        let BlockHashOrTimeout::Block(hash) = value else {
            return Err(RaiVoteStateError::TimeoutFinalVote);
        };
        let targets = self.checked_targets(signer, scope)?;
        if targets
            .iter()
            .any(|&index| self.committees[index].votes.final_locked.contains(&signer))
        {
            return Err(RaiVoteStateError::FinalLocked);
        }

        let final_value = BlockHashOrTimeout::Block(hash);
        if targets.iter().any(|&index| {
            let votes = &self.committees[index].votes;
            votes
                .first
                .get(&signer)
                .is_some_and(|value| *value != final_value)
                || votes
                    .notar
                    .get(&signer)
                    .is_some_and(|values| values.iter().any(|value| *value != final_value))
        }) {
            return Err(RaiVoteStateError::IncompatibleFinalSupport);
        }

        for index in targets {
            let votes = &mut self.committees[index].votes;
            votes.final_votes.insert(signer, hash);
            votes.final_locked.insert(signer);
        }
        Ok(())
    }

    pub fn first_tally(&self, committee: usize, value: BlockHashOrTimeout) -> Amount {
        self.weight_for(committee, |votes, signer| {
            votes.first.get(signer) == Some(&value)
        })
    }

    pub fn notarization_tally(&self, committee: usize, value: BlockHashOrTimeout) -> Amount {
        self.weight_for(committee, |votes, signer| {
            votes.first.get(signer) == Some(&value)
                || votes
                    .notar
                    .get(signer)
                    .is_some_and(|values| values.contains(&value))
        })
    }

    pub fn final_tally(&self, committee: usize, hash: BlockHash) -> Amount {
        self.weight_for(committee, |votes, signer| {
            votes.final_votes.get(signer) == Some(&hash)
        })
    }

    fn checked_targets(
        &self,
        signer: PublicKey,
        scope: RaiCommitteeScope,
    ) -> Result<Vec<usize>, RaiVoteStateError> {
        let targets = self.targets(scope);
        if targets.is_empty() {
            return Err(RaiVoteStateError::EmptyScope);
        }
        if targets
            .iter()
            .any(|&index| self.committees[index].weights.weight(&signer).is_zero())
        {
            return Err(RaiVoteStateError::SignerNotInCommittee);
        }
        Ok(targets)
    }

    fn targets(&self, scope: RaiCommitteeScope) -> Vec<usize> {
        match scope {
            RaiCommitteeScope::All => (0..self.committees.len()).collect(),
            RaiCommitteeScope::Older => (!self.committees.is_empty())
                .then_some(0)
                .into_iter()
                .collect(),
            RaiCommitteeScope::Newer => (!self.committees.is_empty())
                .then_some(self.committees.len() - 1)
                .into_iter()
                .collect(),
        }
    }

    fn weight_for(
        &self,
        committee: usize,
        predicate: impl Fn(&RaiCommitteeVoteState, &PublicKey) -> bool,
    ) -> Amount {
        let Some(instance) = self.committees.get(committee) else {
            return Amount::ZERO;
        };
        instance
            .weights
            .iter()
            .filter(|(signer, _)| predicate(&instance.votes, signer))
            .fold(Amount::ZERO, |total, (_, weight)| total + *weight)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_signers_first_vote() {
        let mut state = state(&[(1, 10)], &[(1, 20)]);
        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();

        assert_eq!(state.first_tally(0, block(1)), Amount::raw(10));
        assert_eq!(state.first_tally(1, block(1)), Amount::raw(20));
    }

    #[test]
    fn first_vote_replay_is_rejected() {
        let mut state = state(&[(1, 10)], &[]);
        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();

        assert_eq!(
            state.record_first_vote(rep(1), block(1), RaiCommitteeScope::All),
            Err(RaiVoteStateError::DuplicateFirstVote)
        );
    }

    #[test]
    fn overlapping_scope_equivocation_is_atomic() {
        let mut state = state(&[(1, 10)], &[(1, 20)]);
        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::Older)
            .unwrap();

        assert_eq!(
            state.record_first_vote(rep(1), block(2), RaiCommitteeScope::All),
            Err(RaiVoteStateError::DuplicateFirstVote)
        );
        assert!(state.committees[1].votes.first.is_empty());
    }

    #[test]
    fn different_first_values_are_allowed_in_disjoint_scopes() {
        let mut state = state(&[(1, 10)], &[(1, 20)]);
        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::Older)
            .unwrap();
        state
            .record_first_vote(rep(1), block(2), RaiCommitteeScope::Newer)
            .unwrap();

        assert_eq!(state.first_tally(0, block(1)), Amount::raw(10));
        assert_eq!(state.first_tally(1, block(2)), Amount::raw(20));
    }

    #[test]
    fn notarization_support_is_committee_local_and_plural() {
        let mut state = state(&[(1, 10)], &[(1, 20)]);
        state
            .record_notarization_vote(rep(1), block(1), RaiCommitteeScope::Older)
            .unwrap();
        state
            .record_notarization_vote(rep(1), block(2), RaiCommitteeScope::Older)
            .unwrap();

        assert_eq!(state.notarization_tally(0, block(1)), Amount::raw(10));
        assert_eq!(state.notarization_tally(0, block(2)), Amount::raw(10));
        assert_eq!(state.notarization_tally(1, block(1)), Amount::ZERO);
    }

    #[test]
    fn valid_final_vote_locks_the_signer() {
        let mut state = state(&[(1, 10)], &[]);
        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_notarization_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_final_vote(rep(1), hash(1), RaiCommitteeScope::All)
            .unwrap();

        assert_eq!(state.final_tally(0, hash(1)), Amount::raw(10));
        assert!(state.committees[0].votes.final_locked.contains(&rep(1)));
    }

    #[test]
    fn final_vote_after_conflicting_support_is_rejected() {
        let mut state = state(&[(1, 10)], &[]);
        state
            .record_notarization_vote(rep(1), block(2), RaiCommitteeScope::All)
            .unwrap();

        assert_eq!(
            state.record_final_vote(rep(1), hash(1), RaiCommitteeScope::All),
            Err(RaiVoteStateError::IncompatibleFinalSupport)
        );
    }

    #[test]
    fn post_final_lock_rejects_every_later_phase() {
        let mut state = state(&[(1, 10)], &[]);
        state
            .record_final_vote(rep(1), hash(1), RaiCommitteeScope::All)
            .unwrap();

        assert_eq!(
            state.record_first_vote(rep(1), block(1), RaiCommitteeScope::All),
            Err(RaiVoteStateError::FinalLocked)
        );
        assert_eq!(
            state.record_notarization_vote(rep(1), block(1), RaiCommitteeScope::All),
            Err(RaiVoteStateError::FinalLocked)
        );
        assert_eq!(
            state.record_final_vote(rep(1), hash(1), RaiCommitteeScope::All),
            Err(RaiVoteStateError::FinalLocked)
        );
    }

    #[test]
    fn signer_must_belong_to_every_targeted_committee() {
        let mut state = state(&[(1, 10)], &[(2, 20)]);

        assert_eq!(
            state.record_first_vote(rep(1), block(1), RaiCommitteeScope::All),
            Err(RaiVoteStateError::SignerNotInCommittee)
        );
        assert!(
            state
                .committees
                .iter()
                .all(|committee| committee.votes.first.is_empty())
        );
    }

    #[test]
    fn signer_weight_is_counted_once_per_phase_and_value() {
        let mut state = state(&[(1, 10)], &[]);
        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_notarization_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_notarization_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();

        assert_eq!(state.notarization_tally(0, block(1)), Amount::raw(10));
    }

    #[test]
    fn duplicate_committees_collapse() {
        let committee = weights(&[(1, 10)]);
        let state = RaiElectionVoteState::new(vec![committee.clone(), committee]);

        assert_eq!(state.committees.len(), 1);
    }

    #[test]
    fn timeout_is_local_state_and_never_a_confirmed_block() {
        let mut first = state(&[(1, 10)], &[]);
        let second = state(&[(1, 10)], &[]);
        first
            .record_first_vote(rep(1), BlockHashOrTimeout::Timeout, RaiCommitteeScope::All)
            .unwrap();

        assert_eq!(
            first.first_tally(0, BlockHashOrTimeout::Timeout),
            Amount::raw(10)
        );
        assert_eq!(
            second.first_tally(0, BlockHashOrTimeout::Timeout),
            Amount::ZERO
        );
        assert_eq!(first.outcome, RaiOutcome::Pending);
        assert_eq!(
            first.record_final_value(rep(1), BlockHashOrTimeout::Timeout, RaiCommitteeScope::All),
            Err(RaiVoteStateError::TimeoutFinalVote)
        );
        assert!(first.committees[0].votes.final_votes.is_empty());
    }

    /* Test helpers */

    fn state(older: &[(u64, u128)], newer: &[(u64, u128)]) -> RaiElectionVoteState {
        let mut committees = vec![weights(older)];
        if !newer.is_empty() {
            committees.push(weights(newer));
        }
        RaiElectionVoteState::new(committees)
    }

    fn weights(entries: &[(u64, u128)]) -> Arc<RepWeights> {
        let mut weights = RepWeights::default();
        for (representative, weight) in entries {
            weights.put(rep(*representative), Amount::raw(*weight));
        }
        Arc::new(weights)
    }

    fn rep(value: u64) -> PublicKey {
        PublicKey::from(value)
    }

    fn hash(value: u64) -> BlockHash {
        BlockHash::from(value)
    }

    fn block(value: u64) -> BlockHashOrTimeout {
        BlockHashOrTimeout::Block(hash(value))
    }
}
