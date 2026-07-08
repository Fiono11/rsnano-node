use std::sync::Arc;

use rsnano_types::{RaiVote, VoteError};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{RaiActiveElections, RaiCommitteeProvider, RepWeightRaiCommitteeProvider};
use crate::representatives::RepresentativeTracker;
use rsnano_ledger::RepWeightCache;

pub struct RaiVoteProcessor {
    active_elections: Arc<RaiActiveElections>,
    rep_tracker: Arc<RepresentativeTracker>,
    committee_provider: Arc<dyn RaiCommitteeProvider>,
    stats: Arc<Stats>,
}

impl RaiVoteProcessor {
    pub fn new(
        active_elections: Arc<RaiActiveElections>,
        rep_tracker: Arc<RepresentativeTracker>,
        rep_weights: Arc<RepWeightCache>,
        stats: Arc<Stats>,
    ) -> Self {
        Self::with_committee_provider(
            active_elections,
            rep_tracker,
            Arc::new(RepWeightRaiCommitteeProvider::new(rep_weights)),
            stats,
        )
    }

    pub fn with_committee_provider(
        active_elections: Arc<RaiActiveElections>,
        rep_tracker: Arc<RepresentativeTracker>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            active_elections,
            rep_tracker,
            committee_provider,
            stats,
        }
    }

    pub fn process(&self, vote: &RaiVote) -> Result<(), VoteError> {
        self.stats
            .inc(StatType::RaiVoteProcessor, DetailType::Process);

        if vote.validate().is_err() {
            self.stats
                .inc(StatType::RaiVoteProcessor, DetailType::Invalid);
            return Err(VoteError::Invalid);
        }

        let committees = self.committee_provider.committees_for(&vote.election_id);

        if !committees.contains(&vote.voter) {
            self.stats
                .inc(StatType::RaiVoteProcessor, DetailType::Ignored);
            return Err(VoteError::Indeterminate);
        }

        if self.active_elections.is_active(&vote.election_id) {
            self.rep_tracker.vote_observed(vote.voter);
        }

        let result = self.active_elections.apply_vote(vote, &committees);

        match result {
            Ok(()) => self
                .stats
                .inc(StatType::RaiVoteProcessor, DetailType::Processed),
            Err(VoteError::Invalid) => self
                .stats
                .inc(StatType::RaiVoteProcessor, DetailType::Invalid),
            Err(VoteError::Replay) => self
                .stats
                .inc(StatType::RaiVoteProcessor, DetailType::Duplicate),
            Err(VoteError::Late) => self
                .stats
                .inc(StatType::RaiVoteProcessor, DetailType::Confirmed),
            Err(VoteError::Indeterminate | VoteError::Ignored | VoteError::Vote) => self
                .stats
                .inc(StatType::RaiVoteProcessor, DetailType::Ignored),
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::representatives::RepresentativeTracker;
    use rsnano_ledger::RepWeightCache;
    use rsnano_types::{
        Account, Amount, BlockHash, PrivateKey, RaiElectionId, RaiElectionValue, RaiSlot,
    };

    #[test]
    fn processes_vote_for_active_election() {
        let fixture = Fixture::new();
        fixture
            .active_elections
            .insert(fixture.election_id.clone())
            .unwrap();
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let vote = RaiVote::new_first(&fixture.rep_key, fixture.election_id.clone(), value.clone());

        assert_eq!(fixture.processor.process(&vote), Ok(()));

        let election = fixture
            .active_elections
            .election(&fixture.election_id)
            .unwrap();
        assert_eq!(election.tally(&value), 1);
    }

    #[test]
    fn unknown_election_is_indeterminate() {
        let fixture = Fixture::new();
        let vote = RaiVote::new_first(
            &fixture.rep_key,
            fixture.election_id.clone(),
            RaiElectionValue::Block(BlockHash::from(3)),
        );

        assert_eq!(
            fixture.processor.process(&vote),
            Err(VoteError::Indeterminate)
        );
    }

    #[test]
    fn invalid_signature_is_rejected_before_routing() {
        let fixture = Fixture::new();
        fixture
            .active_elections
            .insert(fixture.election_id.clone())
            .unwrap();
        let mut vote = RaiVote::new_first(
            &fixture.rep_key,
            fixture.election_id.clone(),
            RaiElectionValue::Block(BlockHash::from(3)),
        );
        vote.value = RaiElectionValue::Block(BlockHash::from(4));

        assert_eq!(fixture.processor.process(&vote), Err(VoteError::Invalid));
    }

    struct Fixture {
        active_elections: Arc<RaiActiveElections>,
        processor: RaiVoteProcessor,
        election_id: RaiElectionId,
        rep_key: PrivateKey,
    }

    impl Fixture {
        fn new() -> Self {
            let rep_key = PrivateKey::from(1);
            let rep_weights = Arc::new(RepWeightCache::default());
            rep_weights.put(rep_key.public_key(), Amount::raw(100));

            let rep_tracker = Arc::new(
                RepresentativeTracker::builder()
                    .rep_weights(rep_weights.clone())
                    .online_weight_minimum(Amount::raw(100))
                    .finish(),
            );

            let active_elections = Arc::new(RaiActiveElections::new());
            let processor = RaiVoteProcessor::new(
                active_elections.clone(),
                rep_tracker,
                rep_weights,
                Arc::new(Stats::default()),
            );

            Self {
                active_elections,
                processor,
                election_id: RaiElectionId::Slot {
                    slot: RaiSlot::new(Account::from(1), 1),
                    epoch: 1,
                },
                rep_key,
            }
        }
    }
}
