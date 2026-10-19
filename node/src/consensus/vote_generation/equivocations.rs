//! RAI evaluation, the unmodified-node baseline: a Byzantine representative
//! that votes for every block of a fork. It is the one Byzantine behaviour
//! this protocol has a phase for; the RAI node plays the same one (its
//! `consensus::byzantine`). Compiled in with the `rai_byzantine` feature only.

use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, QualifiedRoot};

use super::voting_scheduler::VoteTarget;
use crate::consensus::election::VoteType;

/// The votes an equivocating representative cast, watched for a while: a
/// fork sibling often arrives after the vote for the first block
pub(crate) struct Equivocations {
    watched: VecDeque<(Timestamp, VoteTarget)>,
    signed: HashSet<(QualifiedRoot, BlockHash, bool)>,
    signed_order: VecDeque<(QualifiedRoot, BlockHash, bool)>,
}

impl Equivocations {
    /// How long a vote is watched for a sibling
    pub const WATCH: Duration = Duration::from_secs(5);
    const MAX_WATCHED: usize = 64 * 1024;
    const MAX_SIGNED: usize = 64 * 1024;

    pub fn new() -> Self {
        Self {
            watched: VecDeque::new(),
            signed: HashSet::new(),
            signed_order: VecDeque::new(),
        }
    }

    /// The representative voted for `target.winner`
    pub fn voted(&mut self, now: Timestamp, target: VoteTarget) {
        self.mark(
            &target.root,
            target.winner,
            target.vote_type == VoteType::Final,
        );
        if self.watched.len() == Self::MAX_WATCHED {
            self.watched.pop_front();
        }
        self.watched.push_back((now, target));
    }

    /// The (vote, sibling) pairs to sign now, given each watched root's
    /// candidates; votes watched longer than `WATCH` are dropped
    pub fn due(
        &mut self,
        now: Timestamp,
        candidates: impl Fn(&QualifiedRoot) -> Vec<BlockHash>,
    ) -> Vec<(VoteTarget, BlockHash)> {
        while self
            .watched
            .front()
            .is_some_and(|(since, _)| since.elapsed(now) > Self::WATCH)
        {
            self.watched.pop_front();
        }
        let watched: Vec<VoteTarget> = self.watched.iter().map(|(_, t)| t.clone()).collect();
        let mut due = Vec::new();
        for target in watched {
            let finality = target.vote_type == VoteType::Final;
            for sibling in candidates(&target.root) {
                if sibling != target.winner && self.mark(&target.root, sibling, finality) {
                    due.push((target.clone(), sibling));
                }
            }
        }
        due
    }

    /// Whether this (root, block, kind) was new
    fn mark(&mut self, root: &QualifiedRoot, hash: BlockHash, finality: bool) -> bool {
        let key = (root.clone(), hash, finality);
        if !self.signed.insert(key.clone()) {
            return false;
        }
        self.signed_order.push_back(key);
        if self.signed_order.len() > Self::MAX_SIGNED
            && let Some(oldest) = self.signed_order.pop_front()
        {
            self.signed.remove(&oldest);
        }
        true
    }
}

impl Default for Equivocations {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sibling_is_voted_once_whenever_it_arrives() {
        let mut equivocations = Equivocations::new();
        let target = target(1, VoteType::NonFinal);
        equivocations.voted(now(0), target.clone());
        assert!(equivocations.due(now(1), |_| vec![hash(1)]).is_empty());
        let due = equivocations.due(now(2), |_| vec![hash(1), hash(2)]);
        assert_eq!(due, vec![(target, hash(2))]);
        assert!(
            equivocations
                .due(now(3), |_| vec![hash(1), hash(2)])
                .is_empty()
        );
    }

    #[test]
    fn final_votes_are_equivocated_apart_from_non_final_ones() {
        let mut equivocations = Equivocations::new();
        equivocations.voted(now(0), target(1, VoteType::NonFinal));
        equivocations.voted(now(0), target(1, VoteType::Final));
        assert_eq!(
            equivocations.due(now(1), |_| vec![hash(1), hash(2)]).len(),
            2
        );
    }

    #[test]
    fn a_vote_is_watched_for_a_while_only() {
        let mut equivocations = Equivocations::new();
        equivocations.voted(now(0), target(1, VoteType::NonFinal));
        let late = now(0) + Equivocations::WATCH + Duration::from_secs(1);
        assert!(
            equivocations
                .due(late, |_| vec![hash(1), hash(2)])
                .is_empty()
        );
    }

    /* Test helpers */

    fn target(winner: u64, vote_type: VoteType) -> VoteTarget {
        VoteTarget {
            root: QualifiedRoot::new_test_instance(),
            winner: hash(winner),
            vote_type,
        }
    }

    fn hash(i: u64) -> BlockHash {
        BlockHash::from(i)
    }

    fn now(secs: u64) -> Timestamp {
        Timestamp::new_test_instance() + Duration::from_secs(secs)
    }
}
