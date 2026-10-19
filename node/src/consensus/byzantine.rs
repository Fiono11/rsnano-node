//! RAI evaluation: a Byzantine committee member that runs a full node. Its
//! settings name what it does in each phase of the protocol; every phase is
//! honest unless set otherwise. The behaviours are compiled in with the
//! `rai_byzantine` feature only, so the honest nodes of an evaluation run a
//! build without them.

#[cfg(any(test, feature = "rai_byzantine"))]
use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};
use std::{fmt, str::FromStr};

#[cfg(any(test, feature = "rai_byzantine"))]
use rsnano_nullable_clock::Timestamp;
#[cfg(any(test, feature = "rai_byzantine"))]
use rsnano_types::BlockHash;

#[cfg(any(test, feature = "rai_byzantine"))]
use super::{
    election::{ElectionId, VoteType},
    vote_generation::VoteTarget,
};

/// What the member does with its votes on account slots
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VoteAttack {
    #[default]
    Honest,
    /// At a fork position it signs the same kind of vote - first, final -
    /// for every candidate it learns of, not for one
    Equivocate,
}

/// What the member does with its epoch report
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReportAttack {
    #[default]
    Honest,
    /// It signs its report and sends it to no one
    Withhold,
    /// It signs two reports for the epoch and sends one to half of the
    /// representatives, the other to the rest
    Conflicting,
}

/// What the member does in a close round it leads
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LeaderAttack {
    #[default]
    Honest,
    /// It proposes nothing
    Silent,
    /// It proposes two valid values from different report selections, one to
    /// half of the representatives and the other to the rest, and votes
    /// first for both
    Equivocate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByzantineConfig {
    pub votes: VoteAttack,
    pub reports: ReportAttack,
    pub leader: LeaderAttack,
}

impl ByzantineConfig {
    pub fn is_honest(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct UnknownAttack(String);

impl fmt::Display for UnknownAttack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown Byzantine behaviour: {}", self.0)
    }
}

impl FromStr for VoteAttack {
    type Err = UnknownAttack;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "honest" => Ok(Self::Honest),
            "equivocate" => Ok(Self::Equivocate),
            other => Err(UnknownAttack(other.to_owned())),
        }
    }
}

impl FromStr for ReportAttack {
    type Err = UnknownAttack;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "honest" => Ok(Self::Honest),
            "withhold" => Ok(Self::Withhold),
            "conflicting" => Ok(Self::Conflicting),
            other => Err(UnknownAttack(other.to_owned())),
        }
    }
}

impl FromStr for LeaderAttack {
    type Err = UnknownAttack;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "honest" => Ok(Self::Honest),
            "silent" => Ok(Self::Silent),
            "equivocate" => Ok(Self::Equivocate),
            other => Err(UnknownAttack(other.to_owned())),
        }
    }
}

/// Which of two groups a representative falls in, by its position among the
/// peered representatives: a conflicting report or proposal goes to one
/// group in one version and to the other in the other
#[cfg(any(test, feature = "rai_byzantine"))]
pub(crate) fn second_group(position: usize) -> bool {
    position % 2 == 1
}

/// The votes an equivocating member signed on account slots, and the ones it
/// watches: a fork sibling often arrives after the vote for the first block,
/// so a vote is watched for a while and its siblings are voted when they come
#[cfg(any(test, feature = "rai_byzantine"))]
pub(crate) struct Equivocations {
    watched: VecDeque<(Timestamp, VoteTarget)>,
    signed: HashSet<(ElectionId, BlockHash, bool)>,
    signed_order: VecDeque<(ElectionId, BlockHash, bool)>,
}

#[cfg(any(test, feature = "rai_byzantine"))]
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

    /// The member voted for `target.winner`: its siblings are signed too
    pub fn voted(&mut self, now: Timestamp, target: VoteTarget) {
        self.mark(
            &target.election,
            target.winner,
            target.vote_type == VoteType::Final,
        );
        if self.watched.len() == Self::MAX_WATCHED {
            self.watched.pop_front();
        }
        self.watched.push_back((now, target));
    }

    /// The (vote, sibling) pairs to sign now, given each watched election's
    /// candidates; votes watched longer than `WATCH` are dropped
    pub fn due(
        &mut self,
        now: Timestamp,
        candidates: impl Fn(&ElectionId) -> Vec<BlockHash>,
    ) -> Vec<(VoteTarget, BlockHash)> {
        while self
            .watched
            .front()
            .is_some_and(|(since, _)| since.elapsed(now) > Self::WATCH)
        {
            self.watched.pop_front();
        }
        let mut due = Vec::new();
        let watched: Vec<VoteTarget> = self.watched.iter().map(|(_, t)| t.clone()).collect();
        for target in watched {
            let finality = target.vote_type == VoteType::Final;
            for sibling in candidates(&target.election) {
                if sibling != target.winner && self.mark(&target.election, sibling, finality) {
                    due.push((target.clone(), sibling));
                }
            }
        }
        due
    }

    /// Whether this (election, block, kind) was new
    fn mark(&mut self, election: &ElectionId, hash: BlockHash, finality: bool) -> bool {
        let key = (election.clone(), hash, finality);
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

#[cfg(any(test, feature = "rai_byzantine"))]
impl Default for Equivocations {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use rsnano_types::QualifiedRoot;

    use super::*;

    #[test]
    fn behaviours_parse_by_name() {
        assert_eq!("equivocate".parse(), Ok(VoteAttack::Equivocate));
        assert_eq!("conflicting".parse(), Ok(ReportAttack::Conflicting));
        assert_eq!("silent".parse(), Ok(LeaderAttack::Silent));
        assert!("loud".parse::<LeaderAttack>().is_err());
        assert!(ByzantineConfig::default().is_honest());
    }

    #[test]
    fn a_sibling_is_voted_once_whenever_it_arrives() {
        let mut equivocations = Equivocations::new();
        let target = target(1, VoteType::NonFinal);
        equivocations.voted(now(0), target.clone());
        // No sibling yet
        assert!(equivocations.due(now(1), |_| vec![hash(1)]).is_empty());
        let due = equivocations.due(now(2), |_| vec![hash(1), hash(2)]);
        assert_eq!(due, vec![(target.clone(), hash(2))]);
        assert!(
            equivocations
                .due(now(3), |_| vec![hash(1), hash(2)])
                .is_empty()
        );
    }

    #[test]
    fn first_and_final_votes_are_equivocated_apart() {
        let mut equivocations = Equivocations::new();
        equivocations.voted(now(0), target(1, VoteType::NonFinal));
        equivocations.voted(now(0), target(1, VoteType::Final));
        let due = equivocations.due(now(1), |_| vec![hash(1), hash(2)]);
        assert_eq!(due.len(), 2);
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

    #[test]
    fn representatives_are_split_in_two_groups() {
        assert!(!second_group(0));
        assert!(second_group(1));
        assert!(!second_group(4));
    }

    /* Test helpers */

    fn target(winner: u64, vote_type: VoteType) -> VoteTarget {
        VoteTarget {
            election: ElectionId::legacy(QualifiedRoot::new_test_instance()),
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
