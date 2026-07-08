use std::sync::{Arc, RwLock};

use rsnano_types::{RaiElectionId, RaiVote, VoteError};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{
    RaiActiveElections, RaiCloseState, RaiCommitteeProvider, RaiCommitteeSet,
    RepWeightRaiCommitteeProvider,
};
use crate::representatives::RepresentativeTracker;
use rsnano_ledger::RepWeightCache;

pub struct RaiVoteProcessor {
    active_elections: Arc<RaiActiveElections>,
    close_state: Arc<RwLock<RaiCloseState>>,
    rep_tracker: Arc<RepresentativeTracker>,
    committee_provider: Arc<dyn RaiCommitteeProvider>,
    stats: Arc<Stats>,
}

impl RaiVoteProcessor {
    pub fn new(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        rep_tracker: Arc<RepresentativeTracker>,
        rep_weights: Arc<RepWeightCache>,
        stats: Arc<Stats>,
    ) -> Self {
        Self::with_committee_provider(
            active_elections,
            close_state,
            rep_tracker,
            Arc::new(RepWeightRaiCommitteeProvider::new(rep_weights)),
            stats,
        )
    }

    pub fn with_committee_provider(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        rep_tracker: Arc<RepresentativeTracker>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            active_elections,
            close_state,
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

        if let RaiElectionId::Slot { slot, epoch } = &vote.election_id
            && !self
                .close_state
                .read()
                .unwrap()
                .is_slot_vote_enabled(*epoch, slot)
        {
            self.stats
                .inc(StatType::RaiVoteProcessor, DetailType::Ignored);
            return Err(VoteError::Ignored);
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
        if result.is_ok() {
            self.update_vote_visibility(vote, &committees);
        }

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

    fn update_vote_visibility(&self, vote: &RaiVote, committees: &RaiCommitteeSet) {
        let RaiElectionId::Slot { slot, epoch } = &vote.election_id else {
            return;
        };

        let Some(election) = self.active_elections.election(&vote.election_id) else {
            return;
        };

        let visible = committees.iter().any(|committee| {
            let vote_count = election
                .votes()
                .keys()
                .filter(|voter| committee.contains(voter))
                .count();
            committee.has_visibility_quorum(vote_count)
        });

        if visible {
            self.close_state
                .write()
                .unwrap()
                .mark_visible(*epoch, *slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{RaiCommittee, RaiCommitteeDeriver};
    use super::*;
    use crate::representatives::RepresentativeTracker;
    use rsnano_ledger::RepWeightCache;
    use rsnano_types::{
        Account, Amount, BlockHash, PrivateKey, PublicKey, RaiElectionId, RaiElectionValue,
        RaiEpoch, RaiSlot,
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

    #[test]
    fn f_plus_one_votes_in_some_relevant_committee_make_slot_visible() {
        let first_rep = PrivateKey::from(1);
        let second_rep = PrivateKey::from(2);
        let third_rep = PrivateKey::from(3);
        let fourth_rep = PrivateKey::from(4);
        let committee = committee_from_keys([&first_rep, &second_rep, &third_rep, &fourth_rep]);
        assert_eq!(committee.thresholds().max_faulty + 1, 2);
        let fixture = Fixture::with_committee(first_rep.clone(), committee);
        fixture
            .active_elections
            .insert(fixture.election_id.clone())
            .unwrap();
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let slot = RaiSlot::new(Account::from(1), 1);

        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &first_rep,
                fixture.election_id.clone(),
                value.clone()
            )),
            Ok(())
        );
        assert!(!fixture.close_state.read().unwrap().is_visible(1, &slot));

        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &second_rep,
                fixture.election_id.clone(),
                value
            )),
            Ok(())
        );
        assert!(fixture.close_state.read().unwrap().is_visible(1, &slot));
    }

    #[test]
    fn slot_vote_is_ignored_while_closing_until_slot_is_in_cut() {
        let fixture = Fixture::new();
        let slot = RaiSlot::new(Account::from(1), 1);
        let election_id = RaiElectionId::Slot { slot, epoch: 0 };
        fixture
            .close_state
            .write()
            .unwrap()
            .start_closing(0)
            .unwrap();
        fixture
            .active_elections
            .insert(election_id.clone())
            .unwrap();
        let value = RaiElectionValue::Block(BlockHash::from(3));

        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &fixture.rep_key,
                election_id.clone(),
                value.clone()
            )),
            Err(VoteError::Ignored)
        );

        fixture
            .close_state
            .write()
            .unwrap()
            .install_cut(0, [slot].into_iter().collect())
            .unwrap();

        assert_eq!(
            fixture
                .processor
                .process(&RaiVote::new_first(&fixture.rep_key, election_id, value)),
            Ok(())
        );
    }

    struct Fixture {
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        processor: RaiVoteProcessor,
        election_id: RaiElectionId,
        rep_key: PrivateKey,
    }

    impl Fixture {
        fn new() -> Self {
            let rep_key = PrivateKey::from(1);
            Self::with_committee(
                rep_key.clone(),
                committee([(rep_key.public_key(), Amount::raw(100))]),
            )
        }

        fn with_committee(rep_key: PrivateKey, committee: RaiCommittee) -> Self {
            let rep_weights = Arc::new(RepWeightCache::default());
            for member in committee.members() {
                rep_weights.put(member.account, member.balance);
            }

            let rep_tracker = Arc::new(
                RepresentativeTracker::builder()
                    .rep_weights(rep_weights.clone())
                    .online_weight_minimum(Amount::raw(100))
                    .finish(),
            );

            let active_elections = Arc::new(RaiActiveElections::new());
            let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
            let processor = RaiVoteProcessor::with_committee_provider(
                active_elections.clone(),
                close_state.clone(),
                rep_tracker,
                Arc::new(StaticCommitteeProvider::new(committee)),
                Arc::new(Stats::default()),
            );

            Self {
                active_elections,
                close_state,
                processor,
                election_id: RaiElectionId::Slot {
                    slot: RaiSlot::new(Account::from(1), 1),
                    epoch: 1,
                },
                rep_key,
            }
        }
    }

    struct StaticCommitteeProvider {
        committee: RaiCommittee,
    }

    impl StaticCommitteeProvider {
        fn new(committee: RaiCommittee) -> Self {
            Self { committee }
        }
    }

    impl RaiCommitteeProvider for StaticCommitteeProvider {
        fn genesis_committee(&self) -> RaiCommittee {
            self.committee.clone()
        }

        fn committee_for_closed_epoch(&self, _epoch: RaiEpoch) -> Option<RaiCommittee> {
            Some(self.committee.clone())
        }
    }

    fn committee<const N: usize>(values: [(PublicKey, Amount); N]) -> RaiCommittee {
        RaiCommitteeDeriver::new().derive_committee(values)
    }

    fn committee_from_keys<const N: usize>(keys: [&PrivateKey; N]) -> RaiCommittee {
        committee(keys.map(|key| (key.public_key(), Amount::raw(100))))
    }
}
