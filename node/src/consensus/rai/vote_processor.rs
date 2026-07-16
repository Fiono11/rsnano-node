use std::sync::{Arc, RwLock};

use rsnano_types::{
    BlockHash, RaiCloseAttempt, RaiElectionId, RaiElectionValue, RaiEpoch, RaiVote, VoteError,
};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{
    NoopRaiStatePersistence, RaiActiveElections, RaiAdmissibility, RaiAdmissibilityValidator,
    RaiCloseState, RaiClosedSlotState, RaiCommitteeProvider, RaiCommitteeSet,
    RaiDefaultAdmissibilityValidator, RaiElection, RaiElectionInsertError, RaiElectionOutcome,
    RaiElectionStatus, RaiEpochPhase, RaiStatePersistence, RaiVoteSafety,
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

        self.prepare_close_cut_vote_context(vote);
        self.prepare_close_record_vote_context(vote);

        {
            let close_state = self.close_state.read().unwrap();
            if let RaiElectionId::Slot { slot, epoch } = &vote.election_id
                && !close_state.is_slot_vote_acceptable(*epoch, slot)
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

        if self.ensure_election_exists(vote).is_err() {
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
            self.try_install_available_epochs_from_votes();
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

    fn prepare_close_cut_vote_context(&self, vote: &RaiVote) {
        let RaiElectionId::CloseCut { epoch, attempt } = &vote.election_id else {
            return;
        };

        let Some(committees) = self
            .committee_provider
            .try_committees_for(&vote.election_id)
        else {
            return;
        };
        if !committees.contains(&vote.voter) {
            return;
        }

        let mut close_state = self.close_state.write().unwrap();
        if close_state.current_epoch() != *epoch {
            return;
        }

        if vote.value == RaiElectionValue::Timeout {
            if close_state.current_epoch_phase() == RaiEpochPhase::Closing {
                close_state.record_close_attempt_started(*epoch, *attempt);
            }
            return;
        }

        let RaiElectionValue::CloseCutHash(close_hash) = &vote.value else {
            return;
        };

        if close_state.current_epoch_phase() == RaiEpochPhase::Open {
            let _ = close_state.start_closing(*epoch);
        }

        if close_state.close_value(*epoch, close_hash).is_none()
            && close_state.current_close_hash(*epoch) == *close_hash
        {
            close_state.record_current_close_value(*epoch);
        }

        if close_state.close_value(*epoch, close_hash).is_some()
            && close_state.epoch_phase(*epoch) == Some(RaiEpochPhase::Closing)
        {
            close_state.record_close_attempt_started(*epoch, *attempt);
        }
    }

    fn prepare_close_record_vote_context(&self, vote: &RaiVote) {
        let RaiElectionId::CloseRecord { epoch, attempt } = &vote.election_id else {
            return;
        };

        let Some(committees) = self
            .committee_provider
            .try_committees_for(&vote.election_id)
        else {
            return;
        };
        if !committees.contains(&vote.voter) {
            return;
        }

        self.try_install_available_epochs_from_votes();

        let mut close_state = self.close_state.write().unwrap();
        if close_state.current_epoch() != *epoch
            || close_state.epoch_phase(*epoch) != Some(RaiEpochPhase::Closing)
        {
            return;
        }

        if vote.value == RaiElectionValue::Timeout {
            if close_state.cut_drained(*epoch) {
                close_state.record_close_record_attempt_started(*epoch, *attempt);
            }
            return;
        }

        let RaiElectionValue::CloseRecordHash(record_hash) = &vote.value else {
            return;
        };

        if close_state.has_close_record_value(*epoch, record_hash) {
            close_state.record_close_record_attempt_started(*epoch, *attempt);
            return;
        }

        if matches!(
            close_state.current_close_record_hash(*epoch),
            Ok(current_hash) if current_hash == *record_hash
        ) {
            let _ = close_state.record_current_close_record_value(*epoch);
            close_state.record_close_record_attempt_started(*epoch, *attempt);
        }
    }

    fn ensure_election_exists(&self, vote: &RaiVote) -> Result<(), RaiElectionInsertError> {
        if !self.is_raisable_vote(vote) || self.active_elections.contains(&vote.election_id) {
            return Ok(());
        }

        match self.active_elections.insert(vote.election_id.clone()) {
            Ok(()) | Err(RaiElectionInsertError::Duplicate) => Ok(()),
            Err(RaiElectionInsertError::Stopped) => Err(RaiElectionInsertError::Stopped),
        }
    }

    fn is_raisable_vote(&self, vote: &RaiVote) -> bool {
        match (&vote.election_id, &vote.value) {
            (RaiElectionId::Slot { .. }, RaiElectionValue::Block(_))
            | (RaiElectionId::CloseCut { .. }, RaiElectionValue::CloseCutHash(_))
            | (RaiElectionId::CloseRecord { .. }, RaiElectionValue::CloseRecordHash(_)) => true,
            (RaiElectionId::CloseCut { epoch, attempt }, RaiElectionValue::Timeout) => self
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(*epoch, *attempt),
            (RaiElectionId::CloseRecord { epoch, attempt }, RaiElectionValue::Timeout) => self
                .close_state
                .read()
                .unwrap()
                .close_record_attempt_started(*epoch, *attempt),
            _ => false,
        }
    }

    fn try_install_available_epochs_from_votes(&self) {
        while self.try_install_current_epoch_from_votes() {}
    }

    fn try_install_current_epoch_from_votes(&self) -> bool {
        let epoch = self.close_state.read().unwrap().current_epoch();

        self.try_install_close_cut_from_votes(epoch);
        self.try_drain_cut_from_votes(epoch);
        self.try_record_current_close_record_value(epoch);

        self.try_install_close_record_from_votes(epoch)
    }

    fn try_install_close_cut_from_votes(&self, epoch: RaiEpoch) -> bool {
        if self.close_state.read().unwrap().cut_set(epoch).is_some() {
            return true;
        }

        let Some((attempt, close_hash)) = self.fast_close_cut_outcome(epoch) else {
            return false;
        };

        let cut = {
            let close_state = self.close_state.read().unwrap();
            let Some(cut) = close_state.close_value(epoch, &close_hash).cloned() else {
                return false;
            };
            cut
        };
        let cut_for_discard = cut.clone();

        let snapshot = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.current_epoch() != epoch
                || close_state.epoch_phase(epoch) != Some(RaiEpochPhase::Closing)
            {
                return false;
            }

            close_state.record_close_attempt_processed(epoch, attempt);
            match close_state.install_cut(epoch, cut) {
                Ok(true) => Some(close_state.snapshot()),
                Ok(false) => None,
                Err(_) => return false,
            }
        };

        if let Some(snapshot) = snapshot {
            self.active_elections
                .discard_slots_outside_cut(epoch, &cut_for_discard);
            self.persistence.save_close_state(&snapshot);
            tracing::info!(
                "RAI passive close cut installed: epoch={epoch} close_hash={close_hash}"
            );
        }

        true
    }

    fn try_drain_cut_from_votes(&self, epoch: RaiEpoch) -> bool {
        if self.close_state.read().unwrap().cut_drained(epoch) {
            return true;
        }

        let cut = {
            let close_state = self.close_state.read().unwrap();
            let Some(cut) = close_state.cut_set(epoch).cloned() else {
                return false;
            };
            cut
        };

        let mut states = Vec::with_capacity(cut.len());
        for slot in &cut {
            let Some(outcome) = self.slot_outcome(epoch, slot) else {
                return false;
            };
            let Some(state) = closed_slot_state_from_outcome(outcome) else {
                return false;
            };
            states.push((*slot, state));
        }

        let snapshot = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.record_cut_drain(epoch, states).is_err() {
                return false;
            }
            close_state.snapshot()
        };

        self.persistence.save_close_state(&snapshot);
        tracing::info!("RAI passive close cut drained: epoch={epoch}");
        true
    }

    fn try_record_current_close_record_value(&self, epoch: RaiEpoch) -> bool {
        let snapshot = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.current_epoch() != epoch
                || close_state.epoch_phase(epoch) != Some(RaiEpochPhase::Closing)
                || !close_state.cut_drained(epoch)
                || close_state.has_close_record_values(epoch)
            {
                return false;
            }

            if close_state
                .record_current_close_record_value(epoch)
                .is_err()
            {
                return false;
            }
            close_state.snapshot()
        };

        self.persistence.save_close_state(&snapshot);
        true
    }

    fn try_install_close_record_from_votes(&self, epoch: RaiEpoch) -> bool {
        let Some((attempt, record_hash)) = self.fast_close_record_outcome(epoch) else {
            return false;
        };

        let (advanced, snapshot) = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.current_epoch() != epoch
                || !close_state.has_close_record_value(epoch, &record_hash)
            {
                return false;
            }

            close_state.record_close_record_attempt_processed(epoch, attempt);
            if close_state
                .certify_close_record(epoch, &record_hash)
                .is_err()
            {
                return false;
            }

            let advanced = close_state.advance_epoch(epoch).is_ok();
            (advanced, close_state.snapshot())
        };

        if advanced {
            let committee = self
                .committee_provider
                .snapshot_closed_epoch_committee(epoch);
            if let Some(rep_weight_snapshot) = self
                .committee_provider
                .closed_epoch_rep_weight_snapshot(epoch)
            {
                self.persistence
                    .save_rep_weight_snapshot(epoch, &rep_weight_snapshot);
            }
            self.persistence.save_committee_snapshot(epoch, &committee);
            self.persistence.save_close_state(&snapshot);
            tracing::info!(
                "RAI passive close record installed and epoch advanced: epoch={epoch} close_hash={record_hash}"
            );
        } else {
            self.persistence.save_close_state(&snapshot);
        }

        advanced
    }

    fn fast_close_cut_outcome(&self, epoch: RaiEpoch) -> Option<(RaiCloseAttempt, BlockHash)> {
        let mut candidates = Vec::new();
        for election in self.active_elections.snapshot().elections {
            let RaiElectionId::CloseCut {
                epoch: election_epoch,
                attempt,
            } = election.id
            else {
                continue;
            };
            if election_epoch != epoch {
                continue;
            }

            let election_id = RaiElectionId::CloseCut {
                epoch: election_epoch,
                attempt,
            };
            let committees = self.committee_provider.try_committees_for(&election_id)?;
            let election = RaiElection::from_snapshot(election);
            if let Some(RaiElectionOutcome::Fast(RaiElectionValue::CloseCutHash(hash))) =
                election.merged_outcome(&committees)
            {
                candidates.push((attempt, hash));
            }
        }

        candidates.sort_by_key(|(attempt, hash)| (*attempt, *hash));
        candidates.into_iter().next()
    }

    fn fast_close_record_outcome(&self, epoch: RaiEpoch) -> Option<(RaiCloseAttempt, BlockHash)> {
        let mut candidates = Vec::new();
        for election in self.active_elections.snapshot().elections {
            let RaiElectionId::CloseRecord {
                epoch: election_epoch,
                attempt,
            } = election.id
            else {
                continue;
            };
            if election_epoch != epoch {
                continue;
            }

            let election_id = RaiElectionId::CloseRecord {
                epoch: election_epoch,
                attempt,
            };
            let committees = self.committee_provider.try_committees_for(&election_id)?;
            let election = RaiElection::from_snapshot(election);
            if let Some(RaiElectionOutcome::Fast(RaiElectionValue::CloseRecordHash(hash))) =
                election.merged_outcome(&committees)
            {
                candidates.push((attempt, hash));
            }
        }

        candidates.sort_by_key(|(attempt, hash)| (*attempt, *hash));
        candidates.into_iter().next()
    }

    fn slot_outcome(
        &self,
        epoch: RaiEpoch,
        slot: &rsnano_types::RaiSlot,
    ) -> Option<RaiElectionOutcome> {
        let election_id = RaiElectionId::Slot { slot: *slot, epoch };
        let committees = self.committee_provider.try_committees_for(&election_id)?;
        self.active_elections
            .election(&election_id)?
            .merged_outcome(&committees)
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

        let visible = committees.iter().all(|committee| {
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

fn closed_slot_state_from_outcome(outcome: RaiElectionOutcome) -> Option<RaiClosedSlotState> {
    match outcome {
        RaiElectionOutcome::Fast(RaiElectionValue::Block(block))
        | RaiElectionOutcome::Final(RaiElectionValue::Block(block)) => {
            Some(RaiClosedSlotState::Finalized(block))
        }
        RaiElectionOutcome::Notarized(RaiElectionValue::Block(block)) => {
            Some(RaiClosedSlotState::Carry(block))
        }
        RaiElectionOutcome::Timeout => Some(RaiClosedSlotState::Released),
        RaiElectionOutcome::Notarized(_)
        | RaiElectionOutcome::Fast(_)
        | RaiElectionOutcome::Final(_)
        | RaiElectionOutcome::SafetyFault => None,
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
    fn close_cut_timeout_vote_raises_current_closing_attempt() {
        let fixture = Fixture::new();
        fixture
            .close_state
            .write()
            .unwrap()
            .start_closing(0)
            .unwrap();
        let election_id = RaiElectionId::CloseCut {
            epoch: 0,
            attempt: 0,
        };

        assert_eq!(
            fixture.processor.process(&RaiVote::new_notarization(
                &fixture.rep_key,
                election_id.clone(),
                RaiElectionValue::Timeout,
            )),
            Ok(())
        );

        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 0)
        );
        let election = fixture.active_elections.election(&election_id).unwrap();
        let timeout_tally = election
            .snapshot()
            .notarization_tallies
            .into_iter()
            .find(|tally| tally.value == RaiElectionValue::Timeout)
            .unwrap();
        assert_eq!(timeout_tally.per_committee, vec![1]);
    }

    #[test]
    fn non_committee_close_cut_timeout_does_not_start_attempt() {
        let fixture = Fixture::new();
        fixture
            .close_state
            .write()
            .unwrap()
            .start_closing(0)
            .unwrap();
        let outsider_key = PrivateKey::from(2);
        let election_id = RaiElectionId::CloseCut {
            epoch: 0,
            attempt: 0,
        };

        assert_eq!(
            fixture.processor.process(&RaiVote::new_notarization(
                &outsider_key,
                election_id.clone(),
                RaiElectionValue::Timeout,
            )),
            Err(VoteError::Indeterminate)
        );

        assert!(
            !fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 0)
        );
        assert!(fixture.active_elections.election(&election_id).is_none());
    }

    #[test]
    fn close_record_timeout_vote_raises_current_drained_record_attempt() {
        let fixture = Fixture::new();
        {
            let mut close_state = fixture.close_state.write().unwrap();
            close_state.start_closing(0).unwrap();
            close_state.install_cut(0, VisibleSlots::new()).unwrap();
            close_state
                .record_cut_drain(0, std::iter::empty::<(RaiSlot, RaiClosedSlotState)>())
                .unwrap();
        }
        let election_id = RaiElectionId::CloseRecord {
            epoch: 0,
            attempt: 0,
        };

        assert_eq!(
            fixture.processor.process(&RaiVote::new_notarization(
                &fixture.rep_key,
                election_id.clone(),
                RaiElectionValue::Timeout,
            )),
            Ok(())
        );

        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_record_attempt_started(0, 0)
        );
        let election = fixture.active_elections.election(&election_id).unwrap();
        let timeout_tally = election
            .snapshot()
            .notarization_tallies
            .into_iter()
            .find(|tally| tally.value == RaiElectionValue::Timeout)
            .unwrap();
        assert_eq!(timeout_tally.per_committee, vec![1]);
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
    fn f_plus_one_votes_in_relevant_committee_make_slot_visible() {
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
    fn f_plus_one_votes_in_every_relevant_committee_make_slot_visible() {
        let first_rep = PrivateKey::from(1);
        let second_rep = PrivateKey::from(2);
        let fifth_rep = PrivateKey::from(5);
        let first_committee = committee_from_keys([
            &first_rep,
            &second_rep,
            &PrivateKey::from(3),
            &PrivateKey::from(4),
        ]);
        let second_committee = committee_from_keys([
            &first_rep,
            &fifth_rep,
            &PrivateKey::from(6),
            &PrivateKey::from(7),
        ]);
        let active_elections = Arc::new(RaiActiveElections::new());
        let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
        let provider = Arc::new(TwoCommitteeProvider {
            genesis: first_committee,
            closed_zero: second_committee,
        });
        let rep_tracker = Arc::new(RepresentativeTracker::default());
        let processor = RaiVoteProcessor::with_committee_provider(
            active_elections.clone(),
            close_state.clone(),
            rep_tracker,
            provider,
            Arc::new(Stats::default()),
        );
        let slot = RaiSlot::new(Account::from(1), 1);
        let election_id = RaiElectionId::Slot { slot, epoch: 2 };
        active_elections.insert(election_id.clone()).unwrap();
        let value = RaiElectionValue::Block(BlockHash::from(3));

        assert_eq!(
            processor.process(&RaiVote::new_first(
                &first_rep,
                election_id.clone(),
                value.clone()
            )),
            Ok(())
        );
        assert_eq!(
            processor.process(&RaiVote::new_first(
                &second_rep,
                election_id.clone(),
                value.clone()
            )),
            Ok(())
        );
        assert!(!close_state.read().unwrap().is_visible(2, &slot));

        assert_eq!(
            processor.process(&RaiVote::new_first(&fifth_rep, election_id.clone(), value)),
            Ok(())
        );
        assert!(close_state.read().unwrap().is_visible(2, &slot));
    }

    #[test]
    fn slot_vote_is_accepted_passively_while_closing_before_cut() {
        let fixture = Fixture::new();
        let slot = RaiSlot::new(Account::from(1), 1);
        let election_id = RaiElectionId::Slot { slot, epoch: 0 };
        fixture
            .close_state
            .write()
            .unwrap()
            .start_closing(0)
            .unwrap();
        let value = RaiElectionValue::Block(BlockHash::from(3));

        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &fixture.rep_key,
                election_id.clone(),
                value.clone()
            )),
            Ok(())
        );
        assert!(fixture.active_elections.contains(&election_id));
        let election = fixture.active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 1);
    }

    #[test]
    fn slot_vote_is_ignored_after_slot_is_excluded_from_cut() {
        let fixture = Fixture::new();
        let included_slot = RaiSlot::new(Account::from(1), 1);
        let excluded_slot = RaiSlot::new(Account::from(2), 1);
        let excluded_election_id = RaiElectionId::Slot {
            slot: excluded_slot,
            epoch: 0,
        };
        fixture
            .close_state
            .write()
            .unwrap()
            .start_closing(0)
            .unwrap();

        fixture
            .close_state
            .write()
            .unwrap()
            .install_cut(0, [included_slot].into_iter().collect())
            .unwrap();
        let value = RaiElectionValue::Block(BlockHash::from(3));

        assert_eq!(
            fixture.processor.process(&RaiVote::new_first(
                &fixture.rep_key,
                excluded_election_id.clone(),
                value
            )),
            Err(VoteError::Ignored)
        );
        assert!(!fixture.active_elections.contains(&excluded_election_id));
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
    fn accepts_current_close_cut_hash_before_it_was_recorded() {
        let fixture = Fixture::new();
        let visible_slot = RaiSlot::new(Account::from(1), 1);
        let close_hash = {
            let mut close_state = fixture.close_state.write().unwrap();
            close_state.mark_visible(0, visible_slot);
            close_state.current_close_hash(0)
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

        assert_eq!(fixture.processor.process(&vote), Ok(()));

        let close_state = fixture.close_state.read().unwrap();
        assert!(close_state.close_value(0, &close_hash).is_some());
        let election = fixture.active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 1);
    }

    #[test]
    fn accepts_recorded_close_cut_hash_that_omits_later_visible_slot() {
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

        assert_eq!(fixture.processor.process(&vote), Ok(()));

        let election = fixture.active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 1);
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

    #[test]
    fn accepts_current_close_record_hash_before_it_was_recorded() {
        let fixture = Fixture::new();
        let record_hash = {
            let mut close_state = fixture.close_state.write().unwrap();
            close_state.start_closing(0).unwrap();
            close_state.install_cut(0, VisibleSlots::new()).unwrap();
            close_state
                .record_cut_drain(0, std::iter::empty::<(RaiSlot, RaiClosedSlotState)>())
                .unwrap();
            close_state.current_close_record_hash(0).unwrap()
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

        let close_state = fixture.close_state.read().unwrap();
        assert!(close_state.has_close_record_value(0, &record_hash));
        let election = fixture.active_elections.election(&election_id).unwrap();
        assert_eq!(election.tally(&value), 1);
    }

    #[test]
    fn passive_node_installs_epoch_from_committee_close_votes() {
        let fixture = Fixture::new();
        let slot = RaiSlot::new(Account::from(1), 1);
        let slot_election = RaiElectionId::Slot { slot, epoch: 0 };
        let block = BlockHash::from(9);

        fixture
            .processor
            .process(&RaiVote::new_final(
                &fixture.rep_key,
                slot_election,
                RaiElectionValue::Block(block),
            ))
            .unwrap();

        let close_hash = fixture.close_state.read().unwrap().current_close_hash(0);
        fixture
            .processor
            .process(&RaiVote::new_first(
                &fixture.rep_key,
                RaiElectionId::CloseCut {
                    epoch: 0,
                    attempt: 0,
                },
                RaiElectionValue::CloseCutHash(close_hash),
            ))
            .unwrap();

        {
            let close_state = fixture.close_state.read().unwrap();
            assert_eq!(close_state.current_epoch(), 0);
            assert_eq!(close_state.epoch_phase(0), Some(RaiEpochPhase::Closing));
            assert!(
                close_state
                    .cut_set(0)
                    .is_some_and(|cut| cut.contains(&slot))
            );
            assert_eq!(
                close_state.closed_slot_state(0, &slot),
                Some(&RaiClosedSlotState::Finalized(block))
            );
            assert!(close_state.has_close_record_values(0));
        }

        let record_hash = fixture
            .close_state
            .read()
            .unwrap()
            .current_close_record_hash(0)
            .unwrap();
        fixture
            .processor
            .process(&RaiVote::new_first(
                &fixture.rep_key,
                RaiElectionId::CloseRecord {
                    epoch: 0,
                    attempt: 0,
                },
                RaiElectionValue::CloseRecordHash(record_hash),
            ))
            .unwrap();

        let close_state = fixture.close_state.read().unwrap();
        assert_eq!(close_state.current_epoch(), 1);
        assert_eq!(close_state.epoch_phase(0), Some(RaiEpochPhase::Closed));
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

    struct TwoCommitteeProvider {
        genesis: RaiCommittee,
        closed_zero: RaiCommittee,
    }

    impl RaiCommitteeProvider for TwoCommitteeProvider {
        fn genesis_committee(&self) -> RaiCommittee {
            self.genesis.clone()
        }

        fn committee_for_closed_epoch(&self, epoch: RaiEpoch) -> Option<RaiCommittee> {
            (epoch == 0).then(|| self.closed_zero.clone())
        }
    }

    fn committee<const N: usize>(values: [(PublicKey, Amount); N]) -> RaiCommittee {
        RaiCommitteeDeriver::new().derive_committee(values)
    }

    fn committee_from_keys<const N: usize>(keys: [&PrivateKey; N]) -> RaiCommittee {
        committee(keys.map(|key| (key.public_key(), Amount::raw(100))))
    }
}
