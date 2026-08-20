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
    /// Every applicable committee produced a compatible non-timeout
    /// notarization result, but the segment is not yet finalized.
    Notarized(BlockHash),
    Confirmed(BlockHash),
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaiThresholds {
    pub faulty: Amount,
    pub slack: Amount,
    pub progression: Amount,
    pub notarization: Amount,
    pub fast: Amount,
    pub finalization: Amount,
}

pub const fn rai_fault_allowance(total: u128) -> u128 {
    total.saturating_sub(1) / 5
}

pub const fn rai_progression_threshold(faulty: u128, slack: u128) -> u128 {
    faulty.saturating_add(slack).saturating_add(1)
}

pub const fn rai_notarization_threshold(total: u128, faulty: u128, slack: u128) -> u128 {
    total.saturating_sub(faulty.saturating_add(slack))
}

pub const fn rai_fast_threshold(total: u128, slack: u128) -> u128 {
    total.saturating_sub(slack)
}

pub const fn rai_final_threshold(total: u128, faulty: u128, slack: u128) -> u128 {
    rai_notarization_threshold(total, faulty, slack)
}

pub const fn rai_timeout_ready(
    all_first: u128,
    max_first: u128,
    faulty: u128,
    slack: u128,
) -> bool {
    all_first.saturating_sub(max_first) > faulty.saturating_add(slack)
}

impl RaiThresholds {
    pub fn for_weights(weights: &RepWeights) -> Self {
        let total = weights.iter().fold(0u128, |sum, (_, weight)| {
            sum.saturating_add(u128::from_be_bytes(weight.to_be_bytes()))
        });
        let faulty = rai_fault_allowance(total);
        let slack = faulty;
        Self {
            faulty: Amount::raw(faulty),
            slack: Amount::raw(slack),
            progression: Amount::raw(rai_progression_threshold(faulty, slack)),
            notarization: Amount::raw(rai_notarization_threshold(total, faulty, slack)),
            fast: Amount::raw(rai_fast_threshold(total, slack)),
            finalization: Amount::raw(rai_final_threshold(total, faulty, slack)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiLocalResult {
    Notarized(BlockHash),
    Fast(BlockHash),
    Final(BlockHash),
    Timeout,
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
    pub thresholds: RaiThresholds,
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
    InsufficientFirstSupport,
    TimeoutNotReady,
    WrongElectionContext,
}

impl RaiElectionVoteState {
    pub fn has_timeout_certificate(&self, committee: usize) -> bool {
        let Some(instance) = self.committees.get(committee) else {
            return false;
        };
        self.notarization_tally(committee, BlockHashOrTimeout::Timeout)
            >= instance.thresholds.notarization
    }

    pub fn new(committees: Vec<Arc<RepWeights>>) -> Self {
        let mut unique = Vec::new();
        for weights in committees {
            if unique.iter().any(|existing: &RaiCommitteeInstance| {
                Arc::ptr_eq(&existing.weights, &weights) || existing.weights == weights
            }) {
                continue;
            }
            unique.push(RaiCommitteeInstance {
                thresholds: RaiThresholds::for_weights(&weights),
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
        if targets
            .iter()
            .any(|&index| self.committees[index].votes.first.contains_key(&signer))
        {
            return Err(RaiVoteStateError::DuplicateFirstVote);
        }
        // A correctly emitted Final leaf can overtake its earlier First leaf
        // in transport. Accept that delayed leaf only when it is compatible
        // with the already locked Final; conflicting post-Final support stays
        // invalid and can never retract a certificate.
        if targets.iter().any(|&index| {
            let votes = &self.committees[index].votes;
            votes.final_locked.contains(&signer)
                && match value {
                    BlockHashOrTimeout::Block(hash) => {
                        votes.final_votes.get(&signer) != Some(&hash)
                    }
                    BlockHashOrTimeout::Timeout => true,
                }
        }) {
            return Err(RaiVoteStateError::FinalLocked);
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
        if targets.iter().any(|&index| {
            let votes = &self.committees[index].votes;
            votes.final_locked.contains(&signer)
                && match value {
                    BlockHashOrTimeout::Block(hash) => {
                        votes.final_votes.get(&signer) != Some(&hash)
                    }
                    BlockHashOrTimeout::Timeout => true,
                }
        }) {
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
                || match value {
                    BlockHashOrTimeout::Block(hash) => votes.final_votes.get(signer) == Some(&hash),
                    BlockHashOrTimeout::Timeout => false,
                }
        })
    }

    pub fn final_tally(&self, committee: usize, hash: BlockHash) -> Amount {
        self.weight_for(committee, |votes, signer| {
            votes.final_votes.get(signer) == Some(&hash)
        })
    }

    pub fn record_vote(
        &mut self,
        signer: PublicKey,
        value: BlockHashOrTimeout,
        phase: rsnano_types::RaiVotePhase,
        scope: RaiCommitteeScope,
    ) -> Result<(), RaiVoteStateError> {
        if phase == rsnano_types::RaiVotePhase::Notar {
            let targets = self.checked_targets(signer, scope)?;
            match value {
                BlockHashOrTimeout::Block(_) => {
                    if targets.iter().any(|&index| {
                        self.first_tally(index, value)
                            < self.committees[index].thresholds.progression
                    }) {
                        return Err(RaiVoteStateError::InsufficientFirstSupport);
                    }
                }
                BlockHashOrTimeout::Timeout => {
                    if targets.iter().any(|&index| !self.timeout_ready(index)) {
                        return Err(RaiVoteStateError::TimeoutNotReady);
                    }
                }
            }
        }
        match (phase, value) {
            (rsnano_types::RaiVotePhase::First, value) => {
                self.record_first_vote(signer, value, scope)
            }
            (rsnano_types::RaiVotePhase::Notar, value) => {
                self.record_notarization_vote(signer, value, scope)
            }
            (rsnano_types::RaiVotePhase::Final, BlockHashOrTimeout::Block(hash)) => {
                self.record_final_vote(signer, hash, scope)
            }
            (rsnano_types::RaiVotePhase::Final, BlockHashOrTimeout::Timeout) => {
                Err(RaiVoteStateError::TimeoutFinalVote)
            }
        }
    }

    pub fn local_result(&self, committee: usize) -> Option<RaiLocalResult> {
        let instance = self.committees.get(committee)?;
        let mut values = HashSet::new();
        values.extend(instance.votes.first.values().copied());
        for notarized in instance.votes.notar.values() {
            values.extend(notarized.iter().copied());
        }
        values.extend(
            instance
                .votes
                .final_votes
                .values()
                .copied()
                .map(BlockHashOrTimeout::Block),
        );

        let mut notarized = Vec::new();
        let mut fast = Vec::new();
        let mut final_values = Vec::new();
        for value in values {
            if self.notarization_tally(committee, value) >= instance.thresholds.notarization {
                notarized.push(value);
            }
            if let BlockHashOrTimeout::Block(hash) = value {
                if self.first_tally(committee, value) >= instance.thresholds.fast {
                    fast.push(hash);
                }
                if self.final_tally(committee, hash) >= instance.thresholds.finalization {
                    final_values.push(hash);
                }
            }
        }
        final_values.sort();
        final_values.dedup();
        fast.sort();
        fast.dedup();
        notarized.sort_by_key(|value| match value {
            BlockHashOrTimeout::Block(hash) => (0, *hash),
            BlockHashOrTimeout::Timeout => (1, BlockHash::ZERO),
        });
        if final_values.len() > 1
            || fast.len() > 1
            || final_values.first().is_some_and(|final_hash| {
                fast.first()
                    .is_some_and(|fast_hash| fast_hash != final_hash)
            })
        {
            return Some(RaiLocalResult::Timeout);
        }
        let strong = final_values.first().or_else(|| fast.first());
        if strong.is_some_and(|hash| {
            notarized
                .iter()
                .any(|value| *value != BlockHashOrTimeout::Block(*hash))
        }) {
            return Some(RaiLocalResult::Timeout);
        }
        if let Some(hash) = final_values.first() {
            return Some(RaiLocalResult::Final(*hash));
        }
        if let Some(hash) = fast.first() {
            return Some(RaiLocalResult::Fast(*hash));
        }
        if notarized.contains(&BlockHashOrTimeout::Timeout) {
            return Some(RaiLocalResult::Timeout);
        }
        match notarized.as_slice() {
            [BlockHashOrTimeout::Block(hash)] => Some(RaiLocalResult::Notarized(*hash)),
            [_, _, ..] => Some(RaiLocalResult::Timeout),
            _ => None,
        }
    }

    pub fn timeout_ready(&self, committee: usize) -> bool {
        let Some(instance) = self.committees.get(committee) else {
            return false;
        };
        let all_first =
            self.weight_for(committee, |votes, signer| votes.first.contains_key(signer));
        let max_first = instance
            .votes
            .first
            .values()
            .filter_map(|value| match value {
                BlockHashOrTimeout::Block(_) => Some(*value),
                BlockHashOrTimeout::Timeout => None,
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|value| self.first_tally(committee, value))
            .max()
            .unwrap_or_default();
        rai_timeout_ready(
            u128::from_be_bytes(all_first.to_be_bytes()),
            u128::from_be_bytes(max_first.to_be_bytes()),
            u128::from_be_bytes(instance.thresholds.faulty.to_be_bytes()),
            u128::from_be_bytes(instance.thresholds.slack.to_be_bytes()),
        )
    }

    fn checked_targets(
        &self,
        signer: PublicKey,
        scope: RaiCommitteeScope,
    ) -> Result<Vec<usize>, RaiVoteStateError> {
        let targets = match scope {
            RaiCommitteeScope::All => self
                .targets(scope)
                .into_iter()
                .filter(|&index| !self.committees[index].weights.weight(&signer).is_zero())
                .collect(),
            _ => self.targets(scope),
        };
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
    fn exact_weighted_threshold_boundaries() {
        let thresholds =
            RaiThresholds::for_weights(&weights(&[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1)]));
        assert_eq!(thresholds.faulty, Amount::raw(1));
        assert_eq!(thresholds.slack, Amount::raw(1));
        assert_eq!(thresholds.progression, Amount::raw(3)); // strictly > F + P
        assert_eq!(thresholds.notarization, Amount::raw(4));
        assert_eq!(thresholds.fast, Amount::raw(5));
        assert_eq!(thresholds.finalization, Amount::raw(4));
        assert!(!rai_timeout_ready(4, 2, 1, 1));
        assert!(rai_timeout_ready(5, 2, 1, 1));
    }

    #[test]
    fn first_notar_and_final_certificates_use_exact_boundaries() {
        let members = &[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1)];
        let mut vote_state = state(members, &[]);
        for signer in 1..=4 {
            vote_state
                .record_first_vote(rep(signer), block(1), RaiCommitteeScope::All)
                .unwrap();
        }
        assert_eq!(
            vote_state.local_result(0),
            Some(RaiLocalResult::Notarized(hash(1)))
        );
        vote_state
            .record_first_vote(rep(5), block(1), RaiCommitteeScope::All)
            .unwrap();
        assert_eq!(
            vote_state.local_result(0),
            Some(RaiLocalResult::Fast(hash(1)))
        );

        let mut final_state = state(members, &[]);
        for signer in 1..=3 {
            final_state
                .record_final_vote(rep(signer), hash(1), RaiCommitteeScope::All)
                .unwrap();
        }
        assert_eq!(final_state.local_result(0), None);
        final_state
            .record_final_vote(rep(4), hash(1), RaiCommitteeScope::All)
            .unwrap();
        assert_eq!(
            final_state.local_result(0),
            Some(RaiLocalResult::Final(hash(1)))
        );
    }

    #[test]
    fn final_is_implicit_notar_support_but_not_first_support() {
        let members = &[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1)];
        let mut state = state(members, &[]);
        state
            .record_final_vote(rep(1), hash(1), RaiCommitteeScope::All)
            .unwrap();
        for signer in 2..=4 {
            state
                .record_first_vote(rep(signer), block(1), RaiCommitteeScope::All)
                .unwrap();
        }

        assert_eq!(state.first_tally(0, block(1)), Amount::raw(3));
        assert_eq!(state.notarization_tally(0, block(1)), Amount::raw(4));
        assert_eq!(state.final_tally(0, hash(1)), Amount::raw(1));
        assert_eq!(
            state.local_result(0),
            Some(RaiLocalResult::Notarized(hash(1)))
        );
    }

    #[test]
    fn committees_count_the_same_signer_with_local_frozen_weight() {
        let mut state = state(&[(1, 5)], &[(1, 9)]);
        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();
        assert_eq!(state.first_tally(0, block(1)), Amount::raw(5));
        assert_eq!(state.first_tally(1, block(1)), Amount::raw(9));
        assert_eq!(state.local_result(0), Some(RaiLocalResult::Fast(hash(1))));
        assert_eq!(state.local_result(1), Some(RaiLocalResult::Fast(hash(1))));
    }

    #[test]
    fn notarization_requires_strict_first_support_in_every_effective_committee() {
        let mut state = state(
            &[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1)],
            &[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1)],
        );
        for signer in 1..=2 {
            state
                .record_first_vote(rep(signer), block(1), RaiCommitteeScope::All)
                .unwrap();
        }
        assert_eq!(
            state.record_vote(
                rep(3),
                block(1),
                rsnano_types::RaiVotePhase::Notar,
                RaiCommitteeScope::All,
            ),
            Err(RaiVoteStateError::InsufficientFirstSupport)
        );
        state
            .record_first_vote(rep(3), block(1), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_vote(
                rep(4),
                block(1),
                rsnano_types::RaiVotePhase::Notar,
                RaiCommitteeScope::All,
            )
            .unwrap();
    }

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
    fn compatible_earlier_phases_are_order_independent_after_final_arrives() {
        let mut state = state(&[(1, 10)], &[]);
        state
            .record_final_vote(rep(1), hash(1), RaiCommitteeScope::All)
            .unwrap();

        state
            .record_notarization_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();
        assert_eq!(state.first_tally(0, block(1)), Amount::raw(10));
        assert_eq!(state.notarization_tally(0, block(1)), Amount::raw(10));
        assert_eq!(state.final_tally(0, hash(1)), Amount::raw(10));
        assert_eq!(
            state.record_final_vote(rep(1), hash(1), RaiCommitteeScope::All),
            Err(RaiVoteStateError::FinalLocked)
        );
    }

    #[test]
    fn delayed_conflicting_first_cannot_retract_final() {
        let mut state = state(&[(1, 10)], &[]);
        state
            .record_final_vote(rep(1), hash(1), RaiCommitteeScope::All)
            .unwrap();

        assert_eq!(
            state.record_first_vote(rep(1), BlockHashOrTimeout::Timeout, RaiCommitteeScope::All),
            Err(RaiVoteStateError::FinalLocked)
        );
        assert_eq!(state.final_tally(0, hash(1)), Amount::raw(10));
        assert!(state.committees[0].votes.final_locked.contains(&rep(1)));
    }

    #[test]
    fn all_scope_resolves_to_committees_containing_the_signer() {
        let mut state = state(&[(1, 10)], &[(2, 20)]);

        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();
        assert_eq!(state.first_tally(0, block(1)), Amount::raw(10));
        assert_eq!(state.first_tally(1, block(1)), Amount::ZERO);
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

    #[test]
    fn timeout_vote_requires_the_strict_slack_boundary() {
        let mut state = state(&[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1)], &[]);
        state
            .record_first_vote(rep(1), block(1), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_first_vote(rep(2), block(1), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_first_vote(rep(3), block(2), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_first_vote(rep(4), block(2), RaiCommitteeScope::All)
            .unwrap();
        assert_eq!(
            state.record_vote(
                rep(5),
                BlockHashOrTimeout::Timeout,
                rsnano_types::RaiVotePhase::Notar,
                RaiCommitteeScope::All,
            ),
            Err(RaiVoteStateError::TimeoutNotReady)
        );
        state
            .record_first_vote(rep(5), block(3), RaiCommitteeScope::All)
            .unwrap();
        state
            .record_vote(
                rep(6),
                BlockHashOrTimeout::Timeout,
                rsnano_types::RaiVotePhase::Notar,
                RaiCommitteeScope::All,
            )
            .unwrap();
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
