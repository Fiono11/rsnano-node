use std::sync::{Arc, RwLock};

use rsnano_types::{BlockHash, RaiElectionId, RaiElectionValue, RaiVote, VoteError};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{
    NoopRaiStatePersistence, RaiActiveElections, RaiAdmissibility, RaiAdmissibilityValidator,
    RaiCloseState, RaiCommitteeProvider, RaiCommitteeSet, RaiDefaultAdmissibilityValidator,
    RaiElectionInsertError, RaiElectionStatus, RaiStatePersistence, RaiVoteSafety,
    RepWeightRaiCommitteeProvider,
};
use crate::representatives::RepresentativeTracker;
use rsnano_ledger::RepWeightCache;

pub trait RaiSlotConfirmationSink: Send + Sync {
    fn confirm_slot_block(&self, block: BlockHash);
}

struct NoopRaiSlotConfirmationSink;

impl RaiSlotConfirmationSink for NoopRaiSlotConfirmationSink {
    fn confirm_slot_block(&self, _block: BlockHash) {}
}

impl<F> RaiSlotConfirmationSink for F
where
    F: Fn(BlockHash) + Send + Sync,
{
    fn confirm_slot_block(&self, block: BlockHash) {
        self(block);
    }
}

pub struct RaiVoteProcessor {
    active_elections: Arc<RaiActiveElections>,
    close_state: Arc<RwLock<RaiCloseState>>,
    rep_tracker: Arc<RepresentativeTracker>,
    committee_provider: Arc<dyn RaiCommitteeProvider>,
    persistence: Arc<dyn RaiStatePersistence>,
    admissibility: Arc<dyn RaiAdmissibilityValidator>,
    vote_safety: Arc<RwLock<RaiVoteSafety>>,
    slot_confirmation_sink: Arc<dyn RaiSlotConfirmationSink>,
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
        Self::with_committee_provider_and_persistence(
            active_elections,
            close_state,
            rep_tracker,
            committee_provider,
            Arc::new(NoopRaiStatePersistence),
            stats,
        )
    }

    pub fn with_committee_provider_and_persistence(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        rep_tracker: Arc<RepresentativeTracker>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        persistence: Arc<dyn RaiStatePersistence>,
        stats: Arc<Stats>,
    ) -> Self {
        Self::with_committee_provider_persistence_and_admissibility(
            active_elections,
            close_state,
            rep_tracker,
            committee_provider,
            persistence,
            Arc::new(RaiDefaultAdmissibilityValidator),
            stats,
        )
    }

    pub fn with_committee_provider_persistence_and_admissibility(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        rep_tracker: Arc<RepresentativeTracker>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        persistence: Arc<dyn RaiStatePersistence>,
        admissibility: Arc<dyn RaiAdmissibilityValidator>,
        stats: Arc<Stats>,
    ) -> Self {
        Self::with_committee_provider_persistence_admissibility_and_vote_safety(
            active_elections,
            close_state,
            rep_tracker,
            committee_provider,
            persistence,
            admissibility,
            Arc::new(RwLock::new(RaiVoteSafety::new())),
            stats,
        )
    }

    pub fn with_committee_provider_persistence_and_vote_safety(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        rep_tracker: Arc<RepresentativeTracker>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        persistence: Arc<dyn RaiStatePersistence>,
        vote_safety: Arc<RwLock<RaiVoteSafety>>,
        stats: Arc<Stats>,
    ) -> Self {
        Self::with_committee_provider_persistence_admissibility_and_vote_safety(
            active_elections,
            close_state,
            rep_tracker,
            committee_provider,
            persistence,
            Arc::new(RaiDefaultAdmissibilityValidator),
            vote_safety,
            stats,
        )
    }

    pub fn with_committee_provider_persistence_admissibility_and_vote_safety(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        rep_tracker: Arc<RepresentativeTracker>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        persistence: Arc<dyn RaiStatePersistence>,
        admissibility: Arc<dyn RaiAdmissibilityValidator>,
        vote_safety: Arc<RwLock<RaiVoteSafety>>,
        stats: Arc<Stats>,
    ) -> Self {
        Self::with_committee_provider_persistence_admissibility_vote_safety_and_slot_confirmation(
            active_elections,
            close_state,
            rep_tracker,
            committee_provider,
            persistence,
            admissibility,
            vote_safety,
            Arc::new(NoopRaiSlotConfirmationSink),
            stats,
        )
    }

    pub fn with_committee_provider_persistence_admissibility_vote_safety_and_slot_confirmation(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        rep_tracker: Arc<RepresentativeTracker>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        persistence: Arc<dyn RaiStatePersistence>,
        admissibility: Arc<dyn RaiAdmissibilityValidator>,
        vote_safety: Arc<RwLock<RaiVoteSafety>>,
        slot_confirmation_sink: Arc<dyn RaiSlotConfirmationSink>,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            active_elections,
            close_state,
            rep_tracker,
            committee_provider,
            persistence,
            admissibility,
            vote_safety,
            slot_confirmation_sink,
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

        {
            let close_state = self.close_state.read().unwrap();
            if let RaiElectionId::Slot { slot, epoch } = &vote.election_id
                && !close_state.is_slot_vote_enabled(*epoch, slot)
            {
                self.stats
                    .inc(StatType::RaiVoteProcessor, DetailType::Ignored);
                return Err(VoteError::Ignored);
            }

            if RaiAdmissibility::new(&close_state, self.admissibility.as_ref())
                .validate(&vote.election_id, &vote.value)
                .is_err()
            {
                self.stats
                    .inc(StatType::RaiVoteProcessor, DetailType::Invalid);
                return Err(VoteError::Invalid);
            }

            if self
                .vote_safety
                .read()
                .unwrap()
                .validate(&close_state, vote)
                .is_err()
            {
                self.stats
                    .inc(StatType::RaiVoteProcessor, DetailType::Invalid);
                return Err(VoteError::Invalid);
            }
        }

        let Some(committees) = self
            .committee_provider
            .try_committees_for(&vote.election_id)
        else {
            self.stats
                .inc(StatType::RaiVoteProcessor, DetailType::Ignored);
            return Err(VoteError::Indeterminate);
        };

        if !committees.contains(&vote.voter) {
            self.stats
                .inc(StatType::RaiVoteProcessor, DetailType::Ignored);
            return Err(VoteError::Indeterminate);
        }

        if self.ensure_slot_election_exists(vote).is_err() {
            self.stats
                .inc(StatType::RaiVoteProcessor, DetailType::Ignored);
            return Err(VoteError::Ignored);
        }

        let was_confirmed = self
            .active_elections
            .election(&vote.election_id)
            .is_some_and(|election| election.status() == RaiElectionStatus::Confirmed);

        if self.active_elections.is_active(&vote.election_id) {
            self.rep_tracker.vote_observed(vote.voter);
        }

        let result = self.active_elections.apply_vote(vote, &committees);
        if result.is_ok() {
            self.update_vote_visibility(vote, &committees);
            let active_elections = self.active_elections.snapshot();
            let close_state = self.close_state.read().unwrap().snapshot();
            let vote_safety = {
                let mut vote_safety = self.vote_safety.write().unwrap();
                vote_safety.record_vote(vote);
                vote_safety.snapshot()
            };
            self.persistence.save_active_close_and_vote_safety(
                &active_elections,
                &close_state,
                &vote_safety,
            );
            if !was_confirmed {
                self.confirm_slot_block_if_confirmed(&vote.election_id);
            }
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

    fn ensure_slot_election_exists(&self, vote: &RaiVote) -> Result<(), RaiElectionInsertError> {
        if !matches!(
            (&vote.election_id, &vote.value),
            (RaiElectionId::Slot { .. }, RaiElectionValue::Block(_))
        ) || self.active_elections.contains(&vote.election_id)
        {
            return Ok(());
        }

        match self.active_elections.insert(vote.election_id.clone()) {
            Ok(()) | Err(RaiElectionInsertError::Duplicate) => Ok(()),
            Err(RaiElectionInsertError::Stopped) => Err(RaiElectionInsertError::Stopped),
        }
    }

    fn confirm_slot_block_if_confirmed(&self, election_id: &RaiElectionId) {
        let Some(election) = self.active_elections.election(election_id) else {
            return;
        };
        if election.status() != RaiElectionStatus::Confirmed {
            return;
        }

        let Some(RaiElectionValue::Block(block)) = election.confirmed_value() else {
            return;
        };

        self.slot_confirmation_sink.confirm_slot_block(*block);
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
                .voters()
                .iter()
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
    use super::super::{
        RaiAdmissibilityError, RaiClosedSlotState, RaiCommittee, RaiCommitteeDeriver,
        RaiVoteSafetyEntrySnapshot, RaiVoteSafetySnapshot, VisibleSlots,
    };
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
    fn unknown_close_election_is_indeterminate() {
        let fixture = Fixture::new();
        let vote = RaiVote::new_first(
            &fixture.rep_key,
            RaiElectionId::CloseCut {
                epoch: 0,
                attempt: 0,
            },
            RaiElectionValue::Timeout,
        );

        assert_eq!(
            fixture.processor.process(&vote),
            Err(VoteError::Indeterminate)
        );
    }

    #[test]
    fn admissible_slot_vote_starts_missing_election() {
        let fixture = Fixture::new();
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
    fn rejects_slot_block_when_admissibility_validator_rejects_it() {
        let fixture = Fixture::with_admissibility(Arc::new(RejectAllSlotBlocks));
        fixture
            .active_elections
            .insert(fixture.election_id.clone())
            .unwrap();
        let value = RaiElectionValue::Block(BlockHash::from(3));
        let vote = RaiVote::new_first(&fixture.rep_key, fixture.election_id.clone(), value.clone());

        assert_eq!(fixture.processor.process(&vote), Err(VoteError::Invalid));

        let election = fixture
            .active_elections
            .election(&fixture.election_id)
            .unwrap();
        assert_eq!(election.tally(&value), 0);
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

    #[test]
    fn rejects_conflicting_later_same_slot_vote_without_release() {
        let fixture = Fixture::new();
        let slot = RaiSlot::new(Account::from(1), 1);
        let first_election = RaiElectionId::Slot { slot, epoch: 0 };
        let retry_election = RaiElectionId::Slot { slot, epoch: 1 };
        fixture
            .active_elections
            .insert(first_election.clone())
            .unwrap();
        fixture
            .active_elections
            .insert(retry_election.clone())
            .unwrap();
        let first_value = RaiElectionValue::Block(BlockHash::from(3));
        let retry_value = RaiElectionValue::Block(BlockHash::from(4));

        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &fixture.rep_key,
                first_election,
                first_value
            )),
            Ok(())
        );
        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &fixture.rep_key,
                retry_election.clone(),
                retry_value.clone()
            )),
            Err(VoteError::Invalid)
        );

        let retry = fixture.active_elections.election(&retry_election).unwrap();
        assert_eq!(retry.tally(&retry_value), 0);
    }

    #[test]
    fn rejects_conflicting_later_same_slot_vote_from_persisted_safety_history() {
        let rep_key = PrivateKey::from(1);
        let committee = committee([(rep_key.public_key(), Amount::raw(100))]);
        let rep_weights = Arc::new(RepWeightCache::default());
        for member in committee.members() {
            rep_weights.put(member.account, member.balance);
        }
        let rep_tracker = Arc::new(
            RepresentativeTracker::builder()
                .rep_weights(rep_weights)
                .online_weight_minimum(Amount::raw(100))
                .finish(),
        );
        let active_elections = Arc::new(RaiActiveElections::new());
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        let slot = RaiSlot::new(Account::from(1), 1);
        let retry_election = RaiElectionId::Slot { slot, epoch: 1 };
        active_elections.insert(retry_election.clone()).unwrap();
        let vote_safety = Arc::new(RwLock::new(RaiVoteSafety::from_snapshot(
            RaiVoteSafetySnapshot {
                entries: vec![RaiVoteSafetyEntrySnapshot {
                    voter: rep_key.public_key(),
                    slot,
                    epoch: 0,
                    blocks: vec![BlockHash::from(3)],
                }],
            },
        )));
        let processor = RaiVoteProcessor::with_committee_provider_persistence_and_vote_safety(
            active_elections.clone(),
            close_state,
            rep_tracker,
            Arc::new(StaticCommitteeProvider::new(committee)),
            Arc::new(NoopRaiStatePersistence),
            vote_safety,
            Arc::new(Stats::default()),
        );
        let retry_value = RaiElectionValue::Block(BlockHash::from(4));

        assert_eq!(
            processor.process(&RaiVote::new_first(
                &rep_key,
                retry_election.clone(),
                retry_value.clone()
            )),
            Err(VoteError::Invalid)
        );

        let retry = active_elections.election(&retry_election).unwrap();
        assert_eq!(retry.tally(&retry_value), 0);
    }

    #[test]
    fn rejects_vote_when_lagged_committee_history_is_missing() {
        let rep_key = PrivateKey::from(1);
        let committee = committee([(rep_key.public_key(), Amount::raw(100))]);
        let rep_weights = Arc::new(RepWeightCache::default());
        for member in committee.members() {
            rep_weights.put(member.account, member.balance);
        }
        let rep_tracker = Arc::new(
            RepresentativeTracker::builder()
                .rep_weights(rep_weights)
                .online_weight_minimum(Amount::raw(100))
                .finish(),
        );
        let active_elections = Arc::new(RaiActiveElections::new());
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        let election_id = RaiElectionId::Slot {
            slot: RaiSlot::new(Account::from(1), 1),
            epoch: 2,
        };
        active_elections.insert(election_id.clone()).unwrap();
        let processor = RaiVoteProcessor::with_committee_provider(
            active_elections.clone(),
            close_state,
            rep_tracker,
            Arc::new(GenesisOnlyCommitteeProvider::new(committee)),
            Arc::new(Stats::default()),
        );
        let value = RaiElectionValue::Block(BlockHash::from(4));

        assert_eq!(
            processor.process(&RaiVote::new_first(
                &rep_key,
                election_id.clone(),
                value.clone()
            )),
            Err(VoteError::Indeterminate)
        );

        let election = active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 0);
    }

    #[test]
    fn allows_later_same_slot_vote_for_same_block() {
        let fixture = Fixture::new();
        let slot = RaiSlot::new(Account::from(1), 1);
        let first_election = RaiElectionId::Slot { slot, epoch: 0 };
        let retry_election = RaiElectionId::Slot { slot, epoch: 1 };
        fixture
            .active_elections
            .insert(first_election.clone())
            .unwrap();
        fixture
            .active_elections
            .insert(retry_election.clone())
            .unwrap();
        let value = RaiElectionValue::Block(BlockHash::from(3));

        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &fixture.rep_key,
                first_election,
                value.clone()
            )),
            Ok(())
        );
        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &fixture.rep_key,
                retry_election.clone(),
                value.clone()
            )),
            Ok(())
        );

        let retry = fixture.active_elections.election(&retry_election).unwrap();
        assert_eq!(retry.tally(&value), 1);
    }

    #[test]
    fn allows_conflicting_same_slot_vote_after_close_exclusion_release() {
        let fixture = Fixture::new();
        let slot = RaiSlot::new(Account::from(1), 1);
        let first_election = RaiElectionId::Slot { slot, epoch: 0 };
        let retry_election = RaiElectionId::Slot { slot, epoch: 1 };
        fixture
            .active_elections
            .insert(first_election.clone())
            .unwrap();
        fixture
            .active_elections
            .insert(retry_election.clone())
            .unwrap();
        let first_value = RaiElectionValue::Block(BlockHash::from(3));
        let retry_value = RaiElectionValue::Block(BlockHash::from(4));

        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &fixture.rep_key,
                first_election,
                first_value
            )),
            Ok(())
        );
        {
            let mut close_state = fixture.close_state.write().unwrap();
            close_state.start_closing(0).unwrap();
            close_state.install_cut(0, VisibleSlots::new()).unwrap();
            close_state
                .record_cut_drain(0, std::iter::empty::<(RaiSlot, RaiClosedSlotState)>())
                .unwrap();
            close_state.record_current_close_record_value(0).unwrap();
            close_state.advance_epoch(0).unwrap();
        }

        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &fixture.rep_key,
                retry_election.clone(),
                retry_value.clone()
            )),
            Ok(())
        );

        let retry = fixture.active_elections.election(&retry_election).unwrap();
        assert_eq!(retry.tally(&retry_value), 1);
    }

    #[test]
    fn rejects_unknown_close_cut_hash() {
        let fixture = Fixture::new();
        let election_id = RaiElectionId::CloseCut {
            epoch: 0,
            attempt: 0,
        };
        fixture
            .active_elections
            .insert(election_id.clone())
            .unwrap();
        let value = RaiElectionValue::CloseCutHash(BlockHash::from(42));
        let vote = RaiVote::new_first(&fixture.rep_key, election_id.clone(), value.clone());

        assert_eq!(fixture.processor.process(&vote), Err(VoteError::Invalid));

        let election = fixture.active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 0);
    }

    #[test]
    fn accepts_recorded_close_cut_hash() {
        let fixture = Fixture::new();
        let close_hash = fixture
            .close_state
            .write()
            .unwrap()
            .record_current_close_value(0);
        let election_id = RaiElectionId::CloseCut {
            epoch: 0,
            attempt: 0,
        };
        fixture
            .active_elections
            .insert(election_id.clone())
            .unwrap();
        let value = RaiElectionValue::CloseCutHash(close_hash);
        let vote = RaiVote::new_first(&fixture.rep_key, election_id.clone(), value.clone());

        assert_eq!(fixture.processor.process(&vote), Ok(()));

        let election = fixture.active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 1);
    }

    #[test]
    fn rejects_recorded_close_cut_hash_that_omits_new_visible_slot() {
        let fixture = Fixture::new();
        let first_slot = RaiSlot::new(Account::from(1), 1);
        let second_slot = RaiSlot::new(Account::from(2), 1);
        let close_hash = {
            let mut close_state = fixture.close_state.write().unwrap();
            close_state.mark_visible(0, first_slot);
            let close_hash = close_state.record_current_close_value(0);
            close_state.mark_visible(0, second_slot);
            close_hash
        };
        let election_id = RaiElectionId::CloseCut {
            epoch: 0,
            attempt: 0,
        };
        fixture
            .active_elections
            .insert(election_id.clone())
            .unwrap();
        let value = RaiElectionValue::CloseCutHash(close_hash);
        let vote = RaiVote::new_first(&fixture.rep_key, election_id.clone(), value.clone());

        assert_eq!(fixture.processor.process(&vote), Err(VoteError::Invalid));

        let election = fixture.active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 0);
    }

    #[test]
    fn rejects_close_record_hash_without_validated_package() {
        let fixture = Fixture::new();
        let election_id = RaiElectionId::CloseRecord {
            epoch: 0,
            attempt: 0,
        };
        fixture
            .active_elections
            .insert(election_id.clone())
            .unwrap();
        let value = RaiElectionValue::CloseRecordHash(BlockHash::from(42));
        let vote = RaiVote::new_first(&fixture.rep_key, election_id.clone(), value.clone());

        assert_eq!(fixture.processor.process(&vote), Err(VoteError::Invalid));

        let election = fixture.active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 0);
    }

    #[test]
    fn accepts_recorded_close_record_hash() {
        let fixture = Fixture::new();
        let record_hash = {
            let mut close_state = fixture.close_state.write().unwrap();
            close_state.start_closing(0).unwrap();
            close_state.install_cut(0, VisibleSlots::new()).unwrap();
            close_state
                .record_cut_drain(0, std::iter::empty::<(RaiSlot, RaiClosedSlotState)>())
                .unwrap();
            close_state.record_current_close_record_value(0).unwrap()
        };
        let election_id = RaiElectionId::CloseRecord {
            epoch: 0,
            attempt: 0,
        };
        fixture
            .active_elections
            .insert(election_id.clone())
            .unwrap();
        let value = RaiElectionValue::CloseRecordHash(record_hash);
        let vote = RaiVote::new_first(&fixture.rep_key, election_id.clone(), value.clone());

        assert_eq!(fixture.processor.process(&vote), Ok(()));

        let election = fixture.active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 1);
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

        fn with_admissibility(admissibility: Arc<dyn RaiAdmissibilityValidator>) -> Self {
            let rep_key = PrivateKey::from(1);
            Self::with_committee_and_admissibility(
                rep_key.clone(),
                committee([(rep_key.public_key(), Amount::raw(100))]),
                admissibility,
            )
        }

        fn with_committee(rep_key: PrivateKey, committee: RaiCommittee) -> Self {
            Self::with_committee_and_admissibility(
                rep_key,
                committee,
                Arc::new(RaiDefaultAdmissibilityValidator),
            )
        }

        fn with_committee_and_admissibility(
            rep_key: PrivateKey,
            committee: RaiCommittee,
            admissibility: Arc<dyn RaiAdmissibilityValidator>,
        ) -> Self {
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
            let processor = RaiVoteProcessor::with_committee_provider_persistence_and_admissibility(
                active_elections.clone(),
                close_state.clone(),
                rep_tracker,
                Arc::new(StaticCommitteeProvider::new(committee)),
                Arc::new(NoopRaiStatePersistence),
                admissibility,
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

    struct RejectAllSlotBlocks;

    impl RaiAdmissibilityValidator for RejectAllSlotBlocks {
        fn validate_slot_block(
            &self,
            _slot: RaiSlot,
            _epoch: RaiEpoch,
            _block_hash: &BlockHash,
        ) -> Result<(), RaiAdmissibilityError> {
            Err(RaiAdmissibilityError::InadmissibleSlotBlock)
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

    struct GenesisOnlyCommitteeProvider {
        committee: RaiCommittee,
    }

    impl GenesisOnlyCommitteeProvider {
        fn new(committee: RaiCommittee) -> Self {
            Self { committee }
        }
    }

    impl RaiCommitteeProvider for GenesisOnlyCommitteeProvider {
        fn genesis_committee(&self) -> RaiCommittee {
            self.committee.clone()
        }

        fn committee_for_closed_epoch(&self, _epoch: RaiEpoch) -> Option<RaiCommittee> {
            None
        }
    }

    fn committee<const N: usize>(values: [(PublicKey, Amount); N]) -> RaiCommittee {
        RaiCommitteeDeriver::new().derive_committee(values)
    }

    fn committee_from_keys<const N: usize>(keys: [&PrivateKey; N]) -> RaiCommittee {
        committee(keys.map(|key| (key.public_key(), Amount::raw(100))))
    }
}
