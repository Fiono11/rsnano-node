use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use rsnano_ledger::RepWeightCache;
use rsnano_messages::Message;
use rsnano_network::TrafficType;
use rsnano_types::{
    PrivateKey, RaiCloseAttempt, RaiElectionId, RaiElectionValue, RaiEpoch, RaiPendingReport,
    RaiSlot, RaiVote, RaiVoteKind,
};
use rsnano_utils::{CancellationToken, ticker::Tickable};

use super::{
    RaiActiveElections, RaiCloseState, RaiCommitteeProvider, RaiElectionStatus, RaiEpochPhase,
    RaiPendingReportProcessor, RaiVoteProcessor, RepWeightRaiCommitteeProvider, VisibleSlots,
};
use crate::transport::MessageFlooder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaiEpochLoopConfig {
    pub epoch_duration: Duration,
    pub close_attempt_duration: Duration,
    pub tick_interval: Duration,
}

impl Default for RaiEpochLoopConfig {
    fn default() -> Self {
        Self {
            epoch_duration: Duration::from_secs(5 * 60),
            close_attempt_duration: Duration::from_secs(10),
            tick_interval: Duration::from_secs(1),
        }
    }
}

pub trait RaiEpochPublisher: Send + Sync {
    fn publish_pending_report(&self, report: RaiPendingReport);
    fn publish_vote(&self, vote: RaiVote);
}

pub struct RaiNetworkEpochPublisher {
    message_flooder: Arc<Mutex<MessageFlooder>>,
}

impl RaiNetworkEpochPublisher {
    pub fn new(message_flooder: Arc<Mutex<MessageFlooder>>) -> Self {
        Self { message_flooder }
    }
}

impl RaiEpochPublisher for RaiNetworkEpochPublisher {
    fn publish_pending_report(&self, report: RaiPendingReport) {
        self.message_flooder.lock().unwrap().flood(
            &Message::RaiPendingReport(report),
            TrafficType::Generic,
            1.0,
        );
    }

    fn publish_vote(&self, vote: RaiVote) {
        self.message_flooder.lock().unwrap().flood(
            &Message::RaiVote(vote),
            TrafficType::Generic,
            1.0,
        );
    }
}

pub struct RaiEpochLoop {
    active_elections: Arc<RaiActiveElections>,
    close_state: Arc<RwLock<RaiCloseState>>,
    vote_processor: Arc<RaiVoteProcessor>,
    pending_report_processor: Arc<RaiPendingReportProcessor>,
    committee_provider: Arc<dyn RaiCommitteeProvider>,
    local_key: PrivateKey,
    publisher: Arc<dyn RaiEpochPublisher>,
    config: RaiEpochLoopConfig,
    epoch_started_at: Instant,
    close_attempt_started_at: HashMap<(RaiEpoch, RaiCloseAttempt), Instant>,
    timeout_votes_sent: HashSet<(RaiEpoch, RaiCloseAttempt)>,
}

impl RaiEpochLoop {
    pub fn new(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        vote_processor: Arc<RaiVoteProcessor>,
        pending_report_processor: Arc<RaiPendingReportProcessor>,
        rep_weights: Arc<RepWeightCache>,
        local_key: PrivateKey,
        publisher: Arc<dyn RaiEpochPublisher>,
        config: RaiEpochLoopConfig,
    ) -> Self {
        Self::with_committee_provider(
            active_elections,
            close_state,
            vote_processor,
            pending_report_processor,
            Arc::new(RepWeightRaiCommitteeProvider::new(rep_weights)),
            local_key,
            publisher,
            config,
        )
    }

    pub fn with_committee_provider(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        vote_processor: Arc<RaiVoteProcessor>,
        pending_report_processor: Arc<RaiPendingReportProcessor>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        local_key: PrivateKey,
        publisher: Arc<dyn RaiEpochPublisher>,
        config: RaiEpochLoopConfig,
    ) -> Self {
        Self {
            active_elections,
            close_state,
            vote_processor,
            pending_report_processor,
            committee_provider,
            local_key,
            publisher,
            config,
            epoch_started_at: Instant::now(),
            close_attempt_started_at: HashMap::new(),
            timeout_votes_sent: HashSet::new(),
        }
    }

    pub fn config(&self) -> RaiEpochLoopConfig {
        self.config
    }

    pub fn tick_at(&mut self, now: Instant) {
        let epoch = self.close_state.read().unwrap().current_epoch();
        if self.should_start_closing(now) {
            self.start_closing_epoch(epoch);
        }

        if self.close_state.read().unwrap().current_epoch_phase() == RaiEpochPhase::Closing {
            self.drive_closing_epoch(epoch, now);
        }
    }

    fn should_start_closing(&self, now: Instant) -> bool {
        self.close_state.read().unwrap().current_epoch_phase() == RaiEpochPhase::Open
            && now.duration_since(self.epoch_started_at) >= self.config.epoch_duration
    }

    fn start_closing_epoch(&mut self, epoch: RaiEpoch) {
        let started = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.current_epoch_phase() != RaiEpochPhase::Open {
                false
            } else {
                close_state.start_closing(epoch).is_ok()
            }
        };

        if started {
            self.publish_local_pending_report(epoch);
        }
    }

    fn drive_closing_epoch(&mut self, epoch: RaiEpoch, now: Instant) {
        if !self.close_state.read().unwrap().has_close_values(epoch)
            && self.close_reports_ready(epoch)
        {
            self.start_close_attempt(epoch, 0, now);
        }

        self.handle_close_attempt_outcomes(epoch, now);
        self.maybe_timeout_close_attempt(epoch, now);
        self.handle_close_attempt_outcomes(epoch, now);
        self.try_drain_cut(epoch, now);
    }

    fn publish_local_pending_report(&self, epoch: RaiEpoch) {
        if !self.local_can_vote_for_close(epoch, 0) {
            return;
        }

        let report = RaiPendingReport::new(
            &self.local_key,
            epoch,
            self.active_elections.unfinished_slots(epoch),
        );
        if self.pending_report_processor.process(&report).is_ok() {
            self.publisher.publish_pending_report(report);
        }
    }

    fn close_reports_ready(&self, epoch: RaiEpoch) -> bool {
        let election_id = close_election_id(epoch, 0);
        let committees = self.committee_provider.committees_for(&election_id);
        if committees.is_empty() {
            return false;
        }

        let close_state = self.close_state.read().unwrap();
        let reports = close_state.pending_reports(epoch);
        committees.iter().all(|committee| {
            let report_count = reports
                .iter()
                .filter(|report| committee.contains(&report.reporter))
                .count();
            committee.has_fast_quorum(report_count)
        })
    }

    fn start_close_attempt(&mut self, epoch: RaiEpoch, attempt: RaiCloseAttempt, now: Instant) {
        let close_hash = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.close_attempt_started(epoch, attempt) {
                return;
            }

            let close_hash = close_state.record_current_close_value(epoch);
            close_state.record_close_attempt_started(epoch, attempt);
            close_hash
        };

        let election_id = close_election_id(epoch, attempt);
        let _ = self.active_elections.insert(election_id.clone());
        self.close_attempt_started_at.insert((epoch, attempt), now);
        self.timeout_votes_sent.remove(&(epoch, attempt));
        self.publish_local_close_vote(
            RaiVoteKind::First,
            election_id,
            RaiElectionValue::CloseHash(close_hash),
        );
    }

    fn handle_close_attempt_outcomes(&mut self, epoch: RaiEpoch, now: Instant) {
        let attempts = self
            .close_state
            .read()
            .unwrap()
            .started_close_attempts(epoch);
        for attempt in attempts {
            if self
                .close_state
                .read()
                .unwrap()
                .close_attempt_processed(epoch, attempt)
            {
                continue;
            }

            let Some(outcome) = self.close_attempt_outcome(epoch, attempt) else {
                continue;
            };

            if let RaiElectionValue::CloseHash(hash) = &outcome
                && self
                    .close_state
                    .read()
                    .unwrap()
                    .close_value(epoch, hash)
                    .is_none()
            {
                continue;
            }

            self.close_state
                .write()
                .unwrap()
                .record_close_attempt_processed(epoch, attempt);

            match outcome {
                RaiElectionValue::Timeout => {
                    if let Some(next_attempt) = attempt.checked_add(1) {
                        self.start_close_attempt(epoch, next_attempt, now);
                    }
                }
                RaiElectionValue::CloseHash(hash) => self.install_cut(epoch, hash),
                RaiElectionValue::Block(_) => {}
            }
        }
    }

    fn close_attempt_outcome(
        &self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    ) -> Option<RaiElectionValue> {
        let election_id = close_election_id(epoch, attempt);
        let committees = self.committee_provider.committees_for(&election_id);
        let election = self.active_elections.election(&election_id)?;

        if let Some(RaiElectionValue::CloseHash(hash)) = election.fast_value(&committees) {
            return Some(RaiElectionValue::CloseHash(hash));
        }

        election.confirmed_value().cloned()
    }

    fn maybe_timeout_close_attempt(&mut self, epoch: RaiEpoch, now: Instant) {
        let Some(attempt) = self
            .close_state
            .read()
            .unwrap()
            .started_close_attempts(epoch)
            .into_iter()
            .max()
        else {
            return;
        };

        if self
            .close_state
            .read()
            .unwrap()
            .close_attempt_processed(epoch, attempt)
            || self.timeout_votes_sent.contains(&(epoch, attempt))
        {
            return;
        }

        let attempt_started_at = self
            .close_attempt_started_at
            .entry((epoch, attempt))
            .or_insert(now);
        if now.duration_since(*attempt_started_at) < self.config.close_attempt_duration {
            return;
        }

        self.timeout_votes_sent.insert((epoch, attempt));
        self.publish_local_close_vote(
            RaiVoteKind::Final,
            close_election_id(epoch, attempt),
            RaiElectionValue::Timeout,
        );
    }

    fn publish_local_close_vote(
        &self,
        kind: RaiVoteKind,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) {
        let RaiElectionId::Close { epoch, attempt } = election_id else {
            return;
        };

        if !self.local_can_vote_for_close(epoch, attempt) {
            return;
        }

        let election_id = close_election_id(epoch, attempt);
        let vote = match kind {
            RaiVoteKind::First => RaiVote::new_first(&self.local_key, election_id, value),
            RaiVoteKind::Notarization => {
                RaiVote::new_notarization(&self.local_key, election_id, value)
            }
            RaiVoteKind::Final => RaiVote::new_final(&self.local_key, election_id, value),
        };

        if self.vote_processor.process(&vote).is_ok() {
            self.publisher.publish_vote(vote);
        }
    }

    fn local_can_vote_for_close(&self, epoch: RaiEpoch, attempt: RaiCloseAttempt) -> bool {
        self.committee_provider
            .committees_for(&close_election_id(epoch, attempt))
            .contains(&self.local_key.public_key())
    }

    fn install_cut(&self, epoch: RaiEpoch, hash: rsnano_types::BlockHash) {
        let Some(cut) = self
            .close_state
            .read()
            .unwrap()
            .close_value(epoch, &hash)
            .cloned()
        else {
            return;
        };

        let _ = self.close_state.write().unwrap().install_cut(epoch, cut);
    }

    fn try_drain_cut(&mut self, epoch: RaiEpoch, now: Instant) {
        let Some(cut) = self.close_state.read().unwrap().cut_set(epoch).cloned() else {
            return;
        };

        let Some(outcomes) = self.cut_outcomes(epoch, &cut) else {
            return;
        };

        let advanced = {
            let mut close_state = self.close_state.write().unwrap();
            let _ = close_state.record_cut_drain(epoch, outcomes);
            close_state.advance_epoch(epoch).is_ok()
        };

        if advanced {
            self.epoch_started_at = now;
            self.close_attempt_started_at
                .retain(|(attempt_epoch, _), _| *attempt_epoch > epoch);
            self.timeout_votes_sent
                .retain(|(attempt_epoch, _)| *attempt_epoch > epoch);
        }
    }

    fn cut_outcomes(
        &self,
        epoch: RaiEpoch,
        cut: &VisibleSlots,
    ) -> Option<Vec<(RaiSlot, RaiElectionValue)>> {
        let mut outcomes = Vec::with_capacity(cut.len());
        for slot in cut {
            let election_id = RaiElectionId::Slot { slot: *slot, epoch };
            let election = self.active_elections.election(&election_id)?;
            if election.status() != RaiElectionStatus::Confirmed {
                return None;
            }
            outcomes.push((*slot, election.confirmed_value()?.clone()));
        }
        Some(outcomes)
    }
}

impl Tickable for RaiEpochLoop {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        self.tick_at(Instant::now());
    }
}

fn close_election_id(epoch: RaiEpoch, attempt: RaiCloseAttempt) -> RaiElectionId {
    RaiElectionId::Close { epoch, attempt }
}

#[cfg(test)]
mod tests {
    use super::super::{RaiCommittee, RaiCommitteeDeriver};
    use super::*;
    use crate::representatives::RepresentativeTracker;
    use rsnano_types::{Account, Amount, BlockHash, PublicKey};
    use rsnano_utils::stats::Stats;

    #[test]
    fn epoch_timeout_starts_closing_reports_pending_slots_and_starts_close_attempt() {
        let mut fixture = Fixture::single_member();
        fixture
            .active_elections
            .insert(fixture.slot_election_id())
            .unwrap();
        let now = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(now);

        let state = fixture.close_state.read().unwrap();
        assert_eq!(
            state.current_epoch_phase(),
            super::super::RaiEpochPhase::Closing
        );
        assert_eq!(state.pending_report_count(0), 1);
        assert!(state.close_attempt_started(0, 0));
        assert!(state.cut_set(0).unwrap().contains(&fixture.slot));
        assert_eq!(fixture.publisher.reports.lock().unwrap().len(), 1);
        assert_eq!(fixture.publisher.votes.lock().unwrap().len(), 1);
    }

    #[test]
    fn included_cut_drains_confirmed_slots_and_advances_epoch() {
        let mut fixture = Fixture::single_member();
        let slot_election = fixture.slot_election_id();
        let block_value = RaiElectionValue::Block(BlockHash::from(9));
        fixture
            .active_elections
            .insert(slot_election.clone())
            .unwrap();
        fixture.epoch_loop.tick_at(fixture.expired_epoch_time());

        fixture
            .vote_processor
            .process(&RaiVote::new_final(
                &fixture.local_key,
                slot_election,
                block_value.clone(),
            ))
            .unwrap();
        let drain_time = fixture.expired_epoch_time() + Duration::from_secs(1);
        fixture.epoch_loop.tick_at(drain_time);

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.current_epoch(), 1);
        assert_eq!(
            state.epoch_phase(0),
            Some(super::super::RaiEpochPhase::Closed)
        );
        assert_eq!(
            state.closed_slot_outcome(0, &fixture.slot),
            Some(&block_value)
        );
    }

    #[test]
    fn timed_out_close_attempt_retries_with_next_attempt() {
        let mut fixture = Fixture::two_members();
        let other_report = RaiPendingReport::new(&fixture.other_key, 0, Vec::new());
        fixture.epoch_loop.tick_at(fixture.expired_epoch_time());
        fixture
            .pending_report_processor
            .process(&other_report)
            .unwrap();
        let close_start = fixture.expired_epoch_time() + Duration::from_secs(1);
        fixture.epoch_loop.tick_at(close_start);
        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 0)
        );

        fixture
            .epoch_loop
            .tick_at(close_start + fixture.config.close_attempt_duration);
        assert_eq!(
            fixture
                .publisher
                .votes
                .lock()
                .unwrap()
                .iter()
                .filter(|vote| vote.value == RaiElectionValue::Timeout)
                .count(),
            1
        );
        assert!(
            !fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 1)
        );

        fixture
            .vote_processor
            .process(&RaiVote::new_final(
                &fixture.other_key,
                close_election_id(0, 0),
                RaiElectionValue::Timeout,
            ))
            .unwrap();
        fixture
            .epoch_loop
            .tick_at(close_start + fixture.config.close_attempt_duration + Duration::from_secs(1));

        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 1)
        );
    }

    struct Fixture {
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        vote_processor: Arc<RaiVoteProcessor>,
        pending_report_processor: Arc<RaiPendingReportProcessor>,
        epoch_loop: RaiEpochLoop,
        publisher: Arc<CapturingPublisher>,
        local_key: PrivateKey,
        other_key: PrivateKey,
        slot: RaiSlot,
        config: RaiEpochLoopConfig,
    }

    impl Fixture {
        fn single_member() -> Self {
            let local_key = PrivateKey::from(1);
            Self::with_committee(
                local_key.clone(),
                PrivateKey::from(2),
                committee([(local_key.public_key(), Amount::raw(100))]),
            )
        }

        fn two_members() -> Self {
            let local_key = PrivateKey::from(1);
            let other_key = PrivateKey::from(2);
            Self::with_committee(
                local_key.clone(),
                other_key.clone(),
                committee([
                    (local_key.public_key(), Amount::raw(100)),
                    (other_key.public_key(), Amount::raw(100)),
                ]),
            )
        }

        fn with_committee(
            local_key: PrivateKey,
            other_key: PrivateKey,
            committee: RaiCommittee,
        ) -> Self {
            let active_elections = Arc::new(RaiActiveElections::new());
            let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
            let provider = Arc::new(StaticCommitteeProvider { committee });
            let stats = Arc::new(Stats::default());
            let vote_processor = Arc::new(RaiVoteProcessor::with_committee_provider(
                active_elections.clone(),
                close_state.clone(),
                Arc::new(RepresentativeTracker::default()),
                provider.clone(),
                stats.clone(),
            ));
            let pending_report_processor =
                Arc::new(RaiPendingReportProcessor::with_committee_provider(
                    close_state.clone(),
                    provider.clone(),
                    stats,
                ));
            let publisher = Arc::new(CapturingPublisher::default());
            let config = RaiEpochLoopConfig {
                epoch_duration: Duration::from_secs(10),
                close_attempt_duration: Duration::from_secs(5),
                tick_interval: Duration::from_millis(50),
            };
            let mut epoch_loop = RaiEpochLoop::with_committee_provider(
                active_elections.clone(),
                close_state.clone(),
                vote_processor.clone(),
                pending_report_processor.clone(),
                provider,
                local_key.clone(),
                publisher.clone(),
                config,
            );
            epoch_loop.epoch_started_at = Instant::now();

            Self {
                active_elections,
                close_state,
                vote_processor,
                pending_report_processor,
                epoch_loop,
                publisher,
                local_key,
                other_key,
                slot: RaiSlot::new(Account::from(1), 1),
                config,
            }
        }

        fn expired_epoch_time(&self) -> Instant {
            self.epoch_loop.epoch_started_at + self.config.epoch_duration
        }

        fn slot_election_id(&self) -> RaiElectionId {
            RaiElectionId::Slot {
                slot: self.slot,
                epoch: 0,
            }
        }
    }

    #[derive(Default)]
    struct CapturingPublisher {
        reports: Mutex<Vec<RaiPendingReport>>,
        votes: Mutex<Vec<RaiVote>>,
    }

    impl RaiEpochPublisher for CapturingPublisher {
        fn publish_pending_report(&self, report: RaiPendingReport) {
            self.reports.lock().unwrap().push(report);
        }

        fn publish_vote(&self, vote: RaiVote) {
            self.votes.lock().unwrap().push(vote);
        }
    }

    struct StaticCommitteeProvider {
        committee: RaiCommittee,
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
}
