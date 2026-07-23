use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use rsnano_ledger::RepWeightCache;
use rsnano_messages::Message;
use rsnano_network::TrafficType;
use rsnano_types::{
    BlockHash, PrivateKey, RaiCloseAttempt, RaiElectionId, RaiElectionValue, RaiEpoch,
    RaiPendingReport, RaiSlot, RaiVote, RaiVoteKind,
};
use rsnano_utils::{CancellationToken, ticker::Tickable};

use super::{
    NoopRaiStatePersistence, RaiActiveElections, RaiCloseState, RaiClosedSlotState, RaiCommittee,
    RaiCommitteeProvider, RaiElectionOutcome, RaiElectionStatus, RaiEpochPhase,
    RaiPendingReportProcessor, RaiStatePersistence, RaiVoteProcessor,
    RepWeightRaiCommitteeProvider, VisibleSlots,
};
use crate::{transport::MessageFlooder, wallets::WalletRepresentatives};

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
    persistence: Arc<dyn RaiStatePersistence>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    publisher: Arc<dyn RaiEpochPublisher>,
    config: RaiEpochLoopConfig,
    observed_epoch: RaiEpoch,
    epoch_started_at: Instant,
    close_attempt_started_at: HashMap<(RaiEpoch, RaiCloseAttempt), Instant>,
    close_attempt_voted_hash: HashMap<(RaiEpoch, RaiCloseAttempt), BlockHash>,
    timeout_votes_sent: HashSet<(RaiEpoch, RaiCloseAttempt)>,
    cut_installed_at: HashMap<RaiEpoch, Instant>,
    slot_timeout_votes_sent: HashSet<(RaiEpoch, RaiSlot)>,
    close_report_wait_report_counts: HashMap<RaiEpoch, usize>,
    pending_reports_republished_at: HashMap<RaiEpoch, Instant>,
    close_record_attempt_started_at: HashMap<(RaiEpoch, RaiCloseAttempt), Instant>,
    close_record_timeout_votes_sent: HashSet<(RaiEpoch, RaiCloseAttempt)>,
}

impl RaiEpochLoop {
    pub fn new(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        vote_processor: Arc<RaiVoteProcessor>,
        pending_report_processor: Arc<RaiPendingReportProcessor>,
        rep_weights: Arc<RepWeightCache>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        publisher: Arc<dyn RaiEpochPublisher>,
        config: RaiEpochLoopConfig,
    ) -> Self {
        Self::with_committee_provider(
            active_elections,
            close_state,
            vote_processor,
            pending_report_processor,
            Arc::new(RepWeightRaiCommitteeProvider::new(rep_weights)),
            wallet_reps,
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
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        publisher: Arc<dyn RaiEpochPublisher>,
        config: RaiEpochLoopConfig,
    ) -> Self {
        Self::with_committee_provider_and_persistence(
            active_elections,
            close_state,
            vote_processor,
            pending_report_processor,
            committee_provider,
            Arc::new(NoopRaiStatePersistence),
            wallet_reps,
            publisher,
            config,
        )
    }

    pub fn with_committee_provider_and_persistence(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        vote_processor: Arc<RaiVoteProcessor>,
        pending_report_processor: Arc<RaiPendingReportProcessor>,
        committee_provider: Arc<dyn RaiCommitteeProvider>,
        persistence: Arc<dyn RaiStatePersistence>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        publisher: Arc<dyn RaiEpochPublisher>,
        config: RaiEpochLoopConfig,
    ) -> Self {
        let observed_epoch = close_state.read().unwrap().current_epoch();
        Self {
            active_elections,
            close_state,
            vote_processor,
            pending_report_processor,
            committee_provider,
            persistence,
            wallet_reps,
            publisher,
            config,
            observed_epoch,
            epoch_started_at: Instant::now(),
            close_attempt_started_at: HashMap::new(),
            close_attempt_voted_hash: HashMap::new(),
            timeout_votes_sent: HashSet::new(),
            cut_installed_at: HashMap::new(),
            slot_timeout_votes_sent: HashSet::new(),
            close_report_wait_report_counts: HashMap::new(),
            pending_reports_republished_at: HashMap::new(),
            close_record_attempt_started_at: HashMap::new(),
            close_record_timeout_votes_sent: HashSet::new(),
        }
    }

    pub fn config(&self) -> RaiEpochLoopConfig {
        self.config
    }

    pub fn tick_at(&mut self, now: Instant) {
        let epoch = self.close_state.read().unwrap().current_epoch();
        self.sync_epoch_timer(epoch, now);
        if self.should_start_closing(now) {
            self.start_closing_epoch(epoch, now);
        }

        if self.close_state.read().unwrap().current_epoch_phase() == RaiEpochPhase::Closing {
            self.drive_closing_epoch(epoch, now);
        }
    }

    fn should_start_closing(&self, now: Instant) -> bool {
        self.close_state.read().unwrap().current_epoch_phase() == RaiEpochPhase::Open
            && now.duration_since(self.epoch_started_at) >= self.config.epoch_duration
    }

    fn sync_epoch_timer(&mut self, epoch: RaiEpoch, now: Instant) {
        if epoch == self.observed_epoch {
            return;
        }

        self.observed_epoch = epoch;
        self.epoch_started_at = now;
        self.retain_current_epoch_timers(epoch);
    }

    fn retain_current_epoch_timers(&mut self, epoch: RaiEpoch) {
        self.close_attempt_started_at
            .retain(|(attempt_epoch, _), _| *attempt_epoch >= epoch);
        self.close_attempt_voted_hash
            .retain(|(attempt_epoch, _), _| *attempt_epoch >= epoch);
        self.timeout_votes_sent
            .retain(|(attempt_epoch, _)| *attempt_epoch >= epoch);
        self.cut_installed_at
            .retain(|attempt_epoch, _| *attempt_epoch >= epoch);
        self.slot_timeout_votes_sent
            .retain(|(attempt_epoch, _)| *attempt_epoch >= epoch);
        self.close_report_wait_report_counts
            .retain(|attempt_epoch, _| *attempt_epoch >= epoch);
        self.pending_reports_republished_at
            .retain(|attempt_epoch, _| *attempt_epoch >= epoch);
        self.close_record_attempt_started_at
            .retain(|(attempt_epoch, _), _| *attempt_epoch >= epoch);
        self.close_record_timeout_votes_sent
            .retain(|(attempt_epoch, _)| *attempt_epoch >= epoch);
    }

    fn start_closing_epoch(&mut self, epoch: RaiEpoch, now: Instant) {
        let started = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.current_epoch_phase() != RaiEpochPhase::Open {
                false
            } else {
                close_state.start_closing(epoch).is_ok()
            }
        };

        if started {
            self.publish_local_pending_report(epoch, now);
            let snapshot = self.close_state.read().unwrap().snapshot();
            self.persistence.save_close_state(&snapshot);
        }
    }

    fn drive_closing_epoch(&mut self, epoch: RaiEpoch, now: Instant) {
        self.publish_local_pending_report(epoch, now);
        self.republish_known_pending_reports(epoch, now);

        if !self.close_state.read().unwrap().has_close_values(epoch) {
            if self.close_reports_ready(epoch) {
                self.start_close_attempt(epoch, 0, now);
            } else {
                self.log_close_reports_not_ready(epoch);
            }
        }

        self.publish_missing_close_cut_first_votes(epoch, now);
        self.start_updated_close_cut_attempt(epoch, now);
        self.handle_close_attempt_outcomes(epoch, now);
        self.maybe_timeout_close_attempt(epoch, now);
        self.handle_close_attempt_outcomes(epoch, now);
        self.maybe_timeout_cut_slots(epoch, now);
        self.try_drain_cut(epoch, now);
        self.maybe_start_close_record_attempt(epoch, now);
        self.publish_missing_close_record_first_votes(epoch, now);
        self.handle_close_record_attempt_outcomes(epoch, now);
        self.maybe_timeout_close_record_attempt(epoch, now);
        self.handle_close_record_attempt_outcomes(epoch, now);
    }

    fn publish_local_pending_report(&mut self, epoch: RaiEpoch, now: Instant) {
        for key in self.local_rep_keys_for_election(&close_cut_election_id(epoch, 0)) {
            if self
                .close_state
                .read()
                .unwrap()
                .pending_report(epoch, &key.public_key())
                .is_some()
            {
                continue;
            }

            let mut slots = self
                .close_state
                .read()
                .unwrap()
                .visible_slots(epoch)
                .map(|slots| slots.iter().copied().collect::<Vec<_>>())
                .unwrap_or_default();
            slots.sort();
            if slots.len() > RaiPendingReport::MAX_SLOTS {
                tracing::warn!(
                    slots = slots.len(),
                    max_slots = RaiPendingReport::MAX_SLOTS,
                    "RAI pending report truncated to fit payload limit: epoch={epoch}"
                );
                slots.truncate(RaiPendingReport::MAX_SLOTS);
            }

            let report = RaiPendingReport::new(&key, epoch, slots);
            if self.pending_report_processor.process(&report).is_ok() {
                self.publisher.publish_pending_report(report);
                self.pending_reports_republished_at.insert(epoch, now);
            }
        }
    }

    fn republish_known_pending_reports(&mut self, epoch: RaiEpoch, now: Instant) {
        // Reports are the evidence from which every replica derives the close
        // cut. Keep disseminating the complete set after an attempt starts so
        // replicas that initially saw different report quorums can converge on
        // the same visible set. The decided cut is the terminal condition.
        if self.close_state.read().unwrap().cut_set(epoch).is_some() {
            return;
        }

        if self
            .pending_reports_republished_at
            .get(&epoch)
            .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(1))
        {
            return;
        }

        let reports = self
            .close_state
            .read()
            .unwrap()
            .pending_reports(epoch)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if reports.is_empty() {
            return;
        }

        self.pending_reports_republished_at.insert(epoch, now);
        for report in reports {
            self.publisher.publish_pending_report(report);
        }
    }

    fn close_reports_ready(&self, epoch: RaiEpoch) -> bool {
        let election_id = close_cut_election_id(epoch, 0);
        let Some(committees) = self.committee_provider.try_committees_for(&election_id) else {
            return false;
        };
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
            has_close_report_quorum(committee, report_count)
        })
    }

    fn log_close_reports_not_ready(&mut self, epoch: RaiEpoch) {
        const COMMITTEE_HISTORY_MISSING: usize = usize::MAX;
        const EMPTY_COMMITTEE_SET: usize = usize::MAX - 1;

        let election_id = close_cut_election_id(epoch, 0);
        let Some(committees) = self.committee_provider.try_committees_for(&election_id) else {
            if self
                .close_report_wait_report_counts
                .insert(epoch, COMMITTEE_HISTORY_MISSING)
                != Some(COMMITTEE_HISTORY_MISSING)
            {
                tracing::info!("RAI close reports waiting for committee history: epoch={epoch}");
            }
            return;
        };

        if committees.is_empty() {
            if self
                .close_report_wait_report_counts
                .insert(epoch, EMPTY_COMMITTEE_SET)
                != Some(EMPTY_COMMITTEE_SET)
            {
                tracing::info!(
                    "RAI close reports waiting for non-empty committee set: epoch={epoch}"
                );
            }
            return;
        }

        let (pending_report_count, committee_reports) = {
            let close_state = self.close_state.read().unwrap();
            let reports = close_state.pending_reports(epoch);
            let committee_reports = committees
                .iter()
                .enumerate()
                .map(|(index, committee)| {
                    let report_count = reports
                        .iter()
                        .filter(|report| committee.contains(&report.reporter))
                        .count();
                    (
                        index,
                        committee.len(),
                        report_count,
                        close_report_quorum(committee),
                    )
                })
                .collect::<Vec<_>>();
            (reports.len(), committee_reports)
        };

        if pending_report_count == 0
            || self.close_report_wait_report_counts.get(&epoch).copied()
                == Some(pending_report_count)
        {
            return;
        }

        self.close_report_wait_report_counts
            .insert(epoch, pending_report_count);
        tracing::info!(
            pending_reports = pending_report_count,
            ?committee_reports,
            "RAI close reports waiting for report quorum: epoch={epoch}"
        );
    }

    fn start_close_attempt(&mut self, epoch: RaiEpoch, attempt: RaiCloseAttempt, now: Instant) {
        let close_hash = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.close_attempt_started(epoch, attempt) {
                return;
            }

            close_state.record_current_close_value(epoch)
        };

        self.start_close_attempt_with_hash(epoch, attempt, close_hash, now);
    }

    fn start_close_attempt_with_hash(
        &mut self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
        close_hash: BlockHash,
        now: Instant,
    ) {
        let snapshot = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.close_attempt_started(epoch, attempt)
                || close_state.close_value(epoch, &close_hash).is_none()
            {
                return;
            }

            close_state.record_close_attempt_started(epoch, attempt);
            close_state.snapshot()
        };

        let election_id = close_cut_election_id(epoch, attempt);
        let _ = self.active_elections.insert(election_id.clone());
        let active_elections = self.active_elections.snapshot();
        self.persistence
            .save_active_and_close(&active_elections, &snapshot);
        self.close_attempt_started_at.insert((epoch, attempt), now);
        self.timeout_votes_sent.remove(&(epoch, attempt));
        self.close_report_wait_report_counts.remove(&epoch);
        tracing::info!(
            "RAI close cut election started: epoch={epoch} attempt={attempt} close_hash={close_hash}"
        );
        if self.publish_local_vote(
            RaiVoteKind::First,
            election_id,
            RaiElectionValue::CloseCutHash(close_hash),
        ) {
            self.close_attempt_voted_hash
                .insert((epoch, attempt), close_hash);
        }
    }

    fn publish_missing_close_cut_first_votes(&mut self, epoch: RaiEpoch, now: Instant) {
        let votes = {
            let close_state = self.close_state.read().unwrap();
            close_state
                .started_close_attempts(epoch)
                .into_iter()
                .filter(|attempt| !close_state.close_attempt_processed(epoch, *attempt))
                .filter(|attempt| {
                    !self
                        .close_attempt_started_at
                        .contains_key(&(epoch, *attempt))
                })
                .filter_map(|attempt| {
                    let election_id = close_cut_election_id(epoch, attempt);
                    let election = self.active_elections.election(&election_id)?;
                    let RaiElectionValue::CloseCutHash(close_hash) = election.winner().cloned()?
                    else {
                        return None;
                    };

                    if close_state.close_value(epoch, &close_hash).is_none() {
                        return None;
                    }

                    Some((attempt, election_id, close_hash))
                })
                .collect::<Vec<_>>()
        };

        for (attempt, election_id, close_hash) in votes {
            self.close_attempt_started_at.insert((epoch, attempt), now);
            tracing::info!(
                "RAI close cut election joined: epoch={epoch} attempt={attempt} close_hash={close_hash}"
            );
            if self.publish_local_vote(
                RaiVoteKind::First,
                election_id,
                RaiElectionValue::CloseCutHash(close_hash),
            ) {
                self.close_attempt_voted_hash
                    .insert((epoch, attempt), close_hash);
            }
        }
    }

    fn start_updated_close_cut_attempt(&mut self, epoch: RaiEpoch, now: Instant) {
        let Some((latest_attempt, voted_hash)) = ({
            let close_state = self.close_state.read().unwrap();
            if close_state.cut_set(epoch).is_some() {
                return;
            }

            let latest_attempt = match close_state.started_close_attempts(epoch).into_iter().max() {
                Some(attempt) => attempt,
                None => return,
            };

            if close_state.close_attempt_processed(epoch, latest_attempt)
                || self.timeout_votes_sent.contains(&(epoch, latest_attempt))
            {
                return;
            }

            self.close_attempt_voted_hash
                .get(&(epoch, latest_attempt))
                .copied()
                .map(|hash| (latest_attempt, hash))
        }) else {
            return;
        };

        let election_id = close_cut_election_id(epoch, latest_attempt);
        if self
            .active_elections
            .election(&election_id)
            .is_some_and(|election| election.status() == RaiElectionStatus::DrainComplete)
        {
            return;
        }

        let close_hash = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.cut_set(epoch).is_some() {
                return;
            }
            close_state.record_current_close_value(epoch)
        };
        if close_hash == voted_hash {
            return;
        }

        let Some(next_attempt) = latest_attempt.checked_add(1) else {
            return;
        };
        tracing::info!(
            "RAI close cut visible set changed; starting next election: epoch={epoch} attempt={next_attempt} close_hash={close_hash}"
        );
        self.start_close_attempt(epoch, next_attempt, now);
    }

    fn handle_close_attempt_outcomes(&mut self, epoch: RaiEpoch, now: Instant) {
        let attempts = self
            .close_state
            .read()
            .unwrap()
            .started_close_attempts(epoch);
        for attempt in attempts {
            // A fast/final certificate in any round decides the logical close
            // instance. Votes from other rounds can still arrive and be
            // retained, but they must not start successor rounds.
            if self.close_state.read().unwrap().cut_set(epoch).is_some() {
                return;
            }

            let already_processed = self
                .close_state
                .read()
                .unwrap()
                .close_attempt_processed(epoch, attempt);

            let Some(outcome) = self.close_attempt_outcome(epoch, attempt) else {
                continue;
            };

            // Convergence is round-local progress, not a terminal result.
            // Continue observing a processed round so later votes can upgrade
            // it to a fast/final certificate.
            if already_processed && !matches!(outcome, CloseAttemptOutcome::Certified(_)) {
                continue;
            }

            if let CloseAttemptOutcome::Certified(hash) | CloseAttemptOutcome::Converged(hash) =
                outcome
                && self
                    .close_state
                    .read()
                    .unwrap()
                    .close_value(epoch, &hash)
                    .is_none()
            {
                continue;
            }

            {
                let mut close_state = self.close_state.write().unwrap();
                close_state.record_close_attempt_processed(epoch, attempt);
            }

            match outcome {
                CloseAttemptOutcome::Timeout => {
                    tracing::info!(
                        "RAI close cut election timed out: epoch={epoch} attempt={attempt}"
                    );
                    if let Some(next_attempt) = attempt.checked_add(1) {
                        self.start_close_attempt(epoch, next_attempt, now);
                    }
                }
                CloseAttemptOutcome::Certified(hash) => {
                    tracing::info!(
                        "RAI close cut election certified: epoch={epoch} attempt={attempt} close_hash={hash}"
                    );
                    self.install_cut(epoch, hash, now);
                    return;
                }
                CloseAttemptOutcome::Converged(hash) => {
                    tracing::info!(
                        "RAI close cut election converged; carrying value to next attempt: epoch={epoch} attempt={attempt} close_hash={hash}"
                    );
                    if let Some(next_attempt) = attempt.checked_add(1) {
                        self.start_close_attempt_with_hash(epoch, next_attempt, hash, now);
                    }
                }
            }
        }
    }

    fn close_attempt_outcome(
        &self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    ) -> Option<CloseAttemptOutcome> {
        let election_id = close_election_id(epoch, attempt);
        let committees = self.committee_provider.try_committees_for(&election_id)?;
        let election = self.active_elections.election(&election_id)?;

        match election.merged_outcome(&committees)? {
            RaiElectionOutcome::Fast(RaiElectionValue::CloseCutHash(hash)) => {
                Some(CloseAttemptOutcome::Certified(hash))
            }
            RaiElectionOutcome::Notarized(RaiElectionValue::CloseCutHash(hash))
            | RaiElectionOutcome::Final(RaiElectionValue::CloseCutHash(hash)) => {
                Some(CloseAttemptOutcome::Converged(hash))
            }
            RaiElectionOutcome::Timeout => Some(CloseAttemptOutcome::Timeout),
            RaiElectionOutcome::Notarized(_)
            | RaiElectionOutcome::Fast(_)
            | RaiElectionOutcome::Final(_)
            | RaiElectionOutcome::SafetyFault => None,
        }
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

        let elapsed_ms = {
            let attempt_started_at = self
                .close_attempt_started_at
                .entry((epoch, attempt))
                .or_insert(now);
            if now.duration_since(*attempt_started_at) < self.config.close_attempt_duration {
                return;
            }
            now.duration_since(*attempt_started_at).as_millis()
        };

        if self.publish_local_timeout_vote(close_cut_election_id(epoch, attempt)) {
            self.timeout_votes_sent.insert((epoch, attempt));
            tracing::info!(
                elapsed_ms,
                "RAI close cut election timeout vote sent: epoch={epoch} attempt={attempt}"
            );
        }
    }

    fn publish_local_vote(
        &self,
        kind: RaiVoteKind,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> bool {
        let mut published = false;
        for key in self.local_rep_keys_for_election(&election_id) {
            let vote = match kind {
                RaiVoteKind::First => RaiVote::new_first(&key, election_id.clone(), value.clone()),
                RaiVoteKind::Notarization => {
                    RaiVote::new_notarization(&key, election_id.clone(), value.clone())
                }
                RaiVoteKind::Final => RaiVote::new_final(&key, election_id.clone(), value.clone()),
            };

            if self.vote_processor.process(&vote).is_ok() {
                self.publisher.publish_vote(vote);
                published = true;
            }
        }

        published
    }

    fn publish_local_timeout_vote(&self, election_id: RaiElectionId) -> bool {
        let Some(committees) = self.committee_provider.try_committees_for(&election_id) else {
            return false;
        };
        let Some(election) = self.active_elections.election(&election_id) else {
            return false;
        };

        let mut published = false;
        for key in self.local_rep_keys_for_election(&election_id) {
            for committee_index in
                election.timeout_ready_committee_indexes(&key.public_key(), &committees)
            {
                let Ok(committee_index) = u8::try_from(committee_index) else {
                    continue;
                };
                let vote = RaiVote::new_notarization_scoped(
                    &key,
                    committee_index,
                    election_id.clone(),
                    RaiElectionValue::Timeout,
                );
                if self.vote_processor.process(&vote).is_ok() {
                    self.publisher.publish_vote(vote);
                    published = true;
                }
            }
        }

        published
    }

    fn local_rep_keys_for_election(&self, election_id: &RaiElectionId) -> Vec<PrivateKey> {
        let mut rep_keys = Vec::new();
        self.wallet_reps
            .lock()
            .unwrap()
            .rep_priv_keys(&mut rep_keys);

        let Some(committees) = self.committee_provider.try_committees_for(election_id) else {
            return Vec::new();
        };

        rep_keys
            .into_iter()
            .filter(|key| committees.contains(&key.public_key()))
            .collect::<Vec<_>>()
    }

    fn install_cut(&mut self, epoch: RaiEpoch, hash: rsnano_types::BlockHash, now: Instant) {
        let Some(cut) = self
            .close_state
            .read()
            .unwrap()
            .close_value(epoch, &hash)
            .cloned()
        else {
            return;
        };
        let cut_slots = cut.len();
        let cut_for_discard = cut.clone();

        let snapshot = {
            let mut close_state = self.close_state.write().unwrap();
            close_state
                .install_cut(epoch, cut)
                .ok()
                .and_then(|installed| installed.then(|| close_state.snapshot()))
        };

        if let Some(snapshot) = snapshot {
            self.active_elections
                .discard_slots_outside_cut(epoch, &cut_for_discard);
            self.cut_installed_at.entry(epoch).or_insert(now);
            tracing::info!(
                cut_slots,
                "RAI close cut installed: epoch={epoch} close_hash={hash}"
            );
            self.persistence.save_close_state(&snapshot);
        }
    }

    fn maybe_timeout_cut_slots(&mut self, epoch: RaiEpoch, now: Instant) {
        let Some(cut) = self.close_state.read().unwrap().cut_set(epoch).cloned() else {
            return;
        };

        let installed_at = *self.cut_installed_at.entry(epoch).or_insert(now);

        if now.duration_since(installed_at) < self.config.close_attempt_duration {
            return;
        }

        for slot in cut {
            let election_id = RaiElectionId::Slot { slot, epoch };
            if self
                .active_elections
                .election(&election_id)
                .is_some_and(|election| election.status() == RaiElectionStatus::DrainComplete)
            {
                continue;
            }

            if self.slot_timeout_votes_sent.contains(&(epoch, slot)) {
                continue;
            }

            let _ = self.active_elections.insert(election_id.clone());
            if self.publish_local_vote(
                RaiVoteKind::Notarization,
                election_id,
                RaiElectionValue::Timeout,
            ) {
                self.slot_timeout_votes_sent.insert((epoch, slot));
            }
        }
    }

    fn try_drain_cut(&mut self, epoch: RaiEpoch, now: Instant) {
        if self.close_state.read().unwrap().cut_drained(epoch) {
            return;
        }

        let Some(cut) = self.close_state.read().unwrap().cut_set(epoch).cloned() else {
            return;
        };

        let Some(outcomes) = self.cut_outcomes(epoch, &cut) else {
            return;
        };

        let snapshot = {
            let mut close_state = self.close_state.write().unwrap();
            let _ = close_state.record_cut_drain(epoch, outcomes);
            close_state.snapshot()
        };
        tracing::info!("RAI close cut drained: epoch={epoch}");
        self.persistence.save_close_state(&snapshot);
        self.maybe_start_close_record_attempt(epoch, now);
    }

    fn maybe_start_close_record_attempt(&mut self, epoch: RaiEpoch, now: Instant) {
        if !self.close_state.read().unwrap().cut_drained(epoch) {
            return;
        }

        if !self
            .close_state
            .read()
            .unwrap()
            .started_close_record_attempts(epoch)
            .is_empty()
        {
            return;
        }

        self.start_close_record_attempt(epoch, 0, now);
    }

    fn start_close_record_attempt(
        &mut self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
        now: Instant,
    ) {
        let (entries, previous_frontiers) = {
            let close_state = self.close_state.read().unwrap();
            let Ok(entries) = close_state.current_close_record_entries(epoch) else {
                return;
            };
            let Ok(previous_frontiers) = close_state.prior_frontiers(epoch) else {
                return;
            };
            (entries, previous_frontiers.clone())
        };
        let Ok(frontiers) =
            self.vote_processor
                .derive_close_frontiers(epoch, &previous_frontiers, &entries)
        else {
            return;
        };
        let record_hash = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.close_record_attempt_started(epoch, attempt) {
                return;
            }

            let Ok(record_hash) =
                close_state.record_current_close_record_value_with_frontiers(epoch, frontiers)
            else {
                return;
            };
            record_hash
        };

        self.start_close_record_attempt_with_hash(epoch, attempt, record_hash, now);
    }

    fn start_close_record_attempt_with_hash(
        &mut self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
        record_hash: BlockHash,
        now: Instant,
    ) {
        let snapshot = {
            let mut close_state = self.close_state.write().unwrap();
            if close_state.close_record_attempt_started(epoch, attempt)
                || !close_state.has_close_record_value(epoch, &record_hash)
            {
                return;
            }

            close_state.record_close_record_attempt_started(epoch, attempt);
            close_state.snapshot()
        };

        let election_id = close_record_election_id(epoch, attempt);
        let _ = self.active_elections.insert(election_id.clone());
        let active_elections = self.active_elections.snapshot();
        self.persistence
            .save_active_and_close(&active_elections, &snapshot);
        self.close_record_attempt_started_at
            .insert((epoch, attempt), now);
        self.close_record_timeout_votes_sent
            .remove(&(epoch, attempt));
        tracing::info!(
            "RAI close record election started: epoch={epoch} attempt={attempt} record_hash={record_hash}"
        );
        self.publish_local_vote(
            RaiVoteKind::First,
            election_id,
            RaiElectionValue::CloseRecordHash(record_hash),
        );
    }

    fn publish_missing_close_record_first_votes(&mut self, epoch: RaiEpoch, now: Instant) {
        let votes = {
            let close_state = self.close_state.read().unwrap();
            close_state
                .started_close_record_attempts(epoch)
                .into_iter()
                .filter(|attempt| !close_state.close_record_attempt_processed(epoch, *attempt))
                .filter(|attempt| {
                    !self
                        .close_record_attempt_started_at
                        .contains_key(&(epoch, *attempt))
                })
                .filter_map(|attempt| {
                    let election_id = close_record_election_id(epoch, attempt);
                    let election = self.active_elections.election(&election_id)?;
                    let RaiElectionValue::CloseRecordHash(record_hash) =
                        election.winner().cloned()?
                    else {
                        return None;
                    };

                    if !close_state.has_close_record_value(epoch, &record_hash) {
                        return None;
                    }

                    Some((attempt, election_id, record_hash))
                })
                .collect::<Vec<_>>()
        };

        for (attempt, election_id, record_hash) in votes {
            self.close_record_attempt_started_at
                .insert((epoch, attempt), now);
            tracing::info!(
                "RAI close record election joined: epoch={epoch} attempt={attempt} record_hash={record_hash}"
            );
            self.publish_local_vote(
                RaiVoteKind::First,
                election_id,
                RaiElectionValue::CloseRecordHash(record_hash),
            );
        }
    }

    fn handle_close_record_attempt_outcomes(&mut self, epoch: RaiEpoch, now: Instant) {
        let attempts = self
            .close_state
            .read()
            .unwrap()
            .started_close_record_attempts(epoch);
        for attempt in attempts {
            // Once any round installs the close record, the logical close
            // instance is decided. Do not let a late converged outcome from a
            // different round manufacture another successor.
            if self.close_state.read().unwrap().epoch_phase(epoch) == Some(RaiEpochPhase::Closed) {
                return;
            }

            let already_processed = self
                .close_state
                .read()
                .unwrap()
                .close_record_attempt_processed(epoch, attempt);

            let Some(outcome) = self.close_record_attempt_outcome(epoch, attempt) else {
                continue;
            };

            if already_processed && !matches!(outcome, CloseRecordAttemptOutcome::Certified(_)) {
                continue;
            }

            if let CloseRecordAttemptOutcome::Certified(hash)
            | CloseRecordAttemptOutcome::Converged(hash) = outcome
                && !self
                    .close_state
                    .read()
                    .unwrap()
                    .has_close_record_value(epoch, &hash)
            {
                continue;
            }

            {
                let mut close_state = self.close_state.write().unwrap();
                close_state.record_close_record_attempt_processed(epoch, attempt);
            }

            match outcome {
                CloseRecordAttemptOutcome::Timeout => {
                    tracing::info!(
                        "RAI close record election timed out: epoch={epoch} attempt={attempt}"
                    );
                    if let Some(next_attempt) = attempt.checked_add(1) {
                        self.start_close_record_attempt(epoch, next_attempt, now);
                    }
                }
                CloseRecordAttemptOutcome::Certified(hash) => {
                    tracing::info!(
                        "RAI close record election certified: epoch={epoch} attempt={attempt} record_hash={hash}"
                    );
                    self.finish_close_record(epoch, hash, now);
                    return;
                }
                CloseRecordAttemptOutcome::Converged(hash) => {
                    tracing::info!(
                        "RAI close record election converged; carrying value to next attempt: epoch={epoch} attempt={attempt} record_hash={hash}"
                    );
                    if let Some(next_attempt) = attempt.checked_add(1) {
                        self.start_close_record_attempt_with_hash(epoch, next_attempt, hash, now);
                    }
                }
            }
        }
    }

    fn close_record_attempt_outcome(
        &self,
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    ) -> Option<CloseRecordAttemptOutcome> {
        let election_id = close_record_election_id(epoch, attempt);
        let committees = self.committee_provider.try_committees_for(&election_id)?;
        let election = self.active_elections.election(&election_id)?;

        match election.merged_outcome(&committees)? {
            RaiElectionOutcome::Fast(RaiElectionValue::CloseRecordHash(hash)) => {
                Some(CloseRecordAttemptOutcome::Certified(hash))
            }
            RaiElectionOutcome::Notarized(RaiElectionValue::CloseRecordHash(hash))
            | RaiElectionOutcome::Final(RaiElectionValue::CloseRecordHash(hash)) => {
                Some(CloseRecordAttemptOutcome::Converged(hash))
            }
            RaiElectionOutcome::Timeout => Some(CloseRecordAttemptOutcome::Timeout),
            RaiElectionOutcome::Notarized(_)
            | RaiElectionOutcome::Fast(_)
            | RaiElectionOutcome::Final(_)
            | RaiElectionOutcome::SafetyFault => None,
        }
    }

    fn maybe_timeout_close_record_attempt(&mut self, epoch: RaiEpoch, now: Instant) {
        let Some(attempt) = self
            .close_state
            .read()
            .unwrap()
            .started_close_record_attempts(epoch)
            .into_iter()
            .max()
        else {
            return;
        };

        if self
            .close_state
            .read()
            .unwrap()
            .close_record_attempt_processed(epoch, attempt)
            || self
                .close_record_timeout_votes_sent
                .contains(&(epoch, attempt))
        {
            return;
        }

        let elapsed_ms = {
            let attempt_started_at = self
                .close_record_attempt_started_at
                .entry((epoch, attempt))
                .or_insert(now);
            if now.duration_since(*attempt_started_at) < self.config.close_attempt_duration {
                return;
            }
            now.duration_since(*attempt_started_at).as_millis()
        };

        if self.publish_local_timeout_vote(close_record_election_id(epoch, attempt)) {
            self.close_record_timeout_votes_sent
                .insert((epoch, attempt));
            tracing::info!(
                elapsed_ms,
                "RAI close record election timeout vote sent: epoch={epoch} attempt={attempt}"
            );
        }
    }

    fn finish_close_record(
        &mut self,
        epoch: RaiEpoch,
        hash: rsnano_types::BlockHash,
        now: Instant,
    ) {
        if !self
            .close_state
            .read()
            .unwrap()
            .has_close_record_value(epoch, &hash)
        {
            return;
        }

        let (advanced_to, advance_error, snapshot) = {
            let mut close_state = self.close_state.write().unwrap();
            let _ = close_state.certify_close_record(epoch, &hash);
            let advance_result = close_state.advance_epoch(epoch);
            let (advanced_to, advance_error) = match advance_result {
                Ok(advanced_to) => (Some(advanced_to), None),
                Err(error) => (None, Some(error)),
            };
            (advanced_to, advance_error, close_state.snapshot())
        };
        if let Some(advanced_to) = advanced_to {
            tracing::info!(
                "RAI close record installed and epoch advanced: epoch={epoch} close_hash={hash}"
            );
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
            self.sync_epoch_timer(advanced_to, now);
        } else {
            tracing::info!(
                ?advance_error,
                "RAI close record installed without epoch advance: epoch={epoch} close_hash={hash}"
            );
            self.persistence.save_close_state(&snapshot);
        }
    }

    fn cut_outcomes(
        &self,
        epoch: RaiEpoch,
        cut: &VisibleSlots,
    ) -> Option<Vec<(RaiSlot, RaiClosedSlotState)>> {
        let mut states = Vec::with_capacity(cut.len());
        for slot in cut {
            let election_id = RaiElectionId::Slot { slot: *slot, epoch };
            let committees = self.committee_provider.try_committees_for(&election_id)?;
            let election = self.active_elections.election(&election_id)?;
            if election.status() != RaiElectionStatus::DrainComplete {
                return None;
            }
            let state = match election.merged_outcome(&committees)? {
                RaiElectionOutcome::Fast(RaiElectionValue::Block(block))
                | RaiElectionOutcome::Final(RaiElectionValue::Block(block)) => {
                    RaiClosedSlotState::Finalized(block)
                }
                RaiElectionOutcome::Notarized(RaiElectionValue::Block(block)) => {
                    RaiClosedSlotState::Carry(block)
                }
                RaiElectionOutcome::Timeout => RaiClosedSlotState::Released,
                RaiElectionOutcome::Notarized(_)
                | RaiElectionOutcome::Fast(_)
                | RaiElectionOutcome::Final(_)
                | RaiElectionOutcome::SafetyFault => return None,
            };
            states.push((*slot, state));
        }
        Some(states)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseAttemptOutcome {
    Timeout,
    Certified(BlockHash),
    Converged(BlockHash),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseRecordAttemptOutcome {
    Timeout,
    Certified(BlockHash),
    Converged(BlockHash),
}

impl Tickable for RaiEpochLoop {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        self.tick_at(Instant::now());
    }
}

fn close_election_id(epoch: RaiEpoch, attempt: RaiCloseAttempt) -> RaiElectionId {
    close_cut_election_id(epoch, attempt)
}

fn close_cut_election_id(epoch: RaiEpoch, attempt: RaiCloseAttempt) -> RaiElectionId {
    RaiElectionId::CloseCut { epoch, attempt }
}

fn close_record_election_id(epoch: RaiEpoch, attempt: RaiCloseAttempt) -> RaiElectionId {
    RaiElectionId::CloseRecord { epoch, attempt }
}

fn has_close_report_quorum(committee: &RaiCommittee, reports: usize) -> bool {
    !committee.is_empty() && reports >= close_report_quorum(committee)
}

fn close_report_quorum(committee: &RaiCommittee) -> usize {
    committee
        .len()
        .saturating_sub(committee.thresholds().max_faulty)
}

#[cfg(test)]
mod tests {
    use super::super::{RaiCommittee, RaiCommitteeDeriver};
    use super::*;
    use crate::{representatives::RepresentativeTracker, wallets::WalletRepresentatives};
    use rsnano_types::{Account, Amount, BlockHash, PublicKey, WalletId};
    use rsnano_utils::stats::Stats;
    use rsnano_wallet::Wallets;

    #[test]
    fn epoch_timeout_starts_closing_reports_visible_slots_and_starts_close_attempt() {
        let mut fixture = Fixture::single_member();
        fixture
            .active_elections
            .insert(fixture.slot_election_id())
            .unwrap();
        fixture.mark_slot_visible();
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
        assert_eq!(
            fixture.publisher.reports.lock().unwrap()[0].slots,
            vec![fixture.slot]
        );
        assert_eq!(fixture.publisher.votes.lock().unwrap().len(), 1);
    }

    #[test]
    fn installed_close_cut_discards_excluded_slot_elections() {
        let mut fixture = Fixture::single_member();
        let included = fixture.slot_election_id();
        let excluded_slot = RaiSlot::new(Account::from(2), 1);
        let excluded = RaiElectionId::Slot {
            slot: excluded_slot,
            epoch: 0,
        };
        fixture.active_elections.insert(included.clone()).unwrap();
        fixture.active_elections.insert(excluded.clone()).unwrap();
        fixture.mark_slot_visible();

        fixture.epoch_loop.tick_at(fixture.expired_epoch_time());

        let state = fixture.close_state.read().unwrap();
        assert!(state.cut_set(0).unwrap().contains(&fixture.slot));
        assert!(!state.cut_set(0).unwrap().contains(&excluded_slot));
        assert!(fixture.active_elections.contains(&included));
        assert!(!fixture.active_elections.contains(&excluded));
        assert!(
            fixture
                .active_elections
                .contains(&close_cut_election_id(0, 0))
        );
    }

    #[test]
    fn epoch_timeout_waits_for_close_report_quorum() {
        let mut fixture = Fixture::two_members();
        fixture.mark_slot_visible();
        let now = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(now);

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.current_epoch_phase(), RaiEpochPhase::Closing);
        assert_eq!(state.pending_report_count(0), 1);
        assert!(!state.close_attempt_started(0, 0));
        assert!(state.cut_set(0).is_none());
        assert_eq!(fixture.publisher.reports.lock().unwrap().len(), 1);
        assert!(fixture.publisher.votes.lock().unwrap().is_empty());
    }

    #[test]
    fn rebroadcasts_known_pending_reports_while_waiting_for_close_quorum() {
        let mut fixture = Fixture::two_members();
        fixture.mark_slot_visible();
        let now = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(now);
        assert_eq!(fixture.publisher.reports.lock().unwrap().len(), 1);

        fixture.epoch_loop.tick_at(now + Duration::from_millis(999));
        assert_eq!(fixture.publisher.reports.lock().unwrap().len(), 1);

        fixture.epoch_loop.tick_at(now + Duration::from_secs(1));

        let reports = fixture.publisher.reports.lock().unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].reporter, fixture.local_key.public_key());
        assert_eq!(reports[1].reporter, fixture.local_key.public_key());
    }

    #[test]
    fn rebroadcasts_pending_reports_after_close_attempt_starts() {
        let mut fixture = Fixture::two_members();
        fixture.mark_slot_visible();
        let now = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(now);
        fixture
            .pending_report_processor
            .process(&RaiPendingReport::new(
                &fixture.other_key,
                0,
                vec![fixture.slot],
            ))
            .unwrap();
        fixture.epoch_loop.tick_at(now + Duration::from_millis(100));
        {
            let state = fixture.close_state.read().unwrap();
            assert!(state.has_close_values(0));
            assert!(state.cut_set(0).is_none());
        }

        fixture.epoch_loop.tick_at(now + Duration::from_secs(1));

        let reports = fixture.publisher.reports.lock().unwrap();
        assert_eq!(
            reports
                .iter()
                .filter(|report| report.reporter == fixture.local_key.public_key())
                .count(),
            2
        );
        assert_eq!(
            reports
                .iter()
                .filter(|report| report.reporter == fixture.other_key.public_key())
                .count(),
            1
        );
    }

    #[test]
    fn passively_started_close_cut_publishes_local_first_vote() {
        let mut fixture = Fixture::two_members();
        let close_start = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(close_start);
        let close_hash = fixture.close_state.read().unwrap().current_close_hash(0);
        fixture
            .vote_processor
            .process(&RaiVote::new_first(
                &fixture.other_key,
                close_cut_election_id(0, 0),
                RaiElectionValue::CloseCutHash(close_hash),
            ))
            .unwrap();

        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 0)
        );
        assert!(fixture.publisher.votes.lock().unwrap().is_empty());

        fixture
            .epoch_loop
            .tick_at(close_start + Duration::from_secs(1));

        let votes = fixture.publisher.votes.lock().unwrap();
        let close_cut_votes = votes
            .iter()
            .filter(|vote| vote.election_id == close_cut_election_id(0, 0))
            .collect::<Vec<_>>();
        assert_eq!(close_cut_votes.len(), 1);
        assert_eq!(close_cut_votes[0].voter, fixture.local_key.public_key());
        assert_eq!(close_cut_votes[0].kind, RaiVoteKind::First);
        assert_eq!(
            close_cut_votes[0].value,
            RaiElectionValue::CloseCutHash(close_hash)
        );
    }

    #[test]
    fn passively_started_close_cut_timeout_waits_until_timeout_is_ready() {
        let mut fixture = Fixture::two_members();
        let close_start = fixture.expired_epoch_time();

        fixture
            .close_state
            .write()
            .unwrap()
            .start_closing(0)
            .unwrap();
        fixture
            .vote_processor
            .process(&RaiVote::new_notarization(
                &fixture.other_key,
                close_cut_election_id(0, 0),
                RaiElectionValue::Timeout,
            ))
            .unwrap();

        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 0)
        );

        fixture.epoch_loop.tick_at(close_start);
        fixture
            .epoch_loop
            .tick_at(close_start + fixture.config.close_attempt_duration);

        let votes = fixture.publisher.votes.lock().unwrap();
        assert!(votes.iter().all(|vote| {
            vote.election_id != close_cut_election_id(0, 0)
                || vote.kind != RaiVoteKind::Notarization
                || vote.value != RaiElectionValue::Timeout
        }));
    }

    #[test]
    fn close_cut_update_uses_next_attempt_when_visible_set_expands() {
        let mut fixture = Fixture::two_members();
        fixture.mark_slot_visible();
        let close_start = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(close_start);
        fixture
            .pending_report_processor
            .process(&RaiPendingReport::new(
                &fixture.other_key,
                0,
                vec![fixture.slot],
            ))
            .unwrap();
        fixture
            .epoch_loop
            .tick_at(close_start + Duration::from_secs(1));

        let first_hash = fixture.close_state.read().unwrap().current_close_hash(0);
        let second_slot = RaiSlot::new(Account::from(2), 1);
        fixture
            .close_state
            .write()
            .unwrap()
            .mark_visible(0, second_slot);
        let updated_hash = fixture.close_state.read().unwrap().current_close_hash(0);
        assert_ne!(first_hash, updated_hash);

        fixture
            .epoch_loop
            .tick_at(close_start + Duration::from_secs(2));

        let votes = fixture.publisher.votes.lock().unwrap();
        let first_attempt_votes = votes
            .iter()
            .filter(|vote| {
                vote.election_id == close_cut_election_id(0, 0) && vote.kind == RaiVoteKind::First
            })
            .collect::<Vec<_>>();
        let second_attempt_votes = votes
            .iter()
            .filter(|vote| {
                vote.election_id == close_cut_election_id(0, 1) && vote.kind == RaiVoteKind::First
            })
            .collect::<Vec<_>>();
        assert_eq!(first_attempt_votes.len(), 1);
        assert_eq!(second_attempt_votes.len(), 1);
        assert_eq!(
            first_attempt_votes[0].value,
            RaiElectionValue::CloseCutHash(first_hash)
        );
        assert_eq!(
            second_attempt_votes[0].value,
            RaiElectionValue::CloseCutHash(updated_hash)
        );

        let first_attempt = fixture
            .active_elections
            .election(&close_cut_election_id(0, 0))
            .unwrap();
        let second_attempt = fixture
            .active_elections
            .election(&close_cut_election_id(0, 1))
            .unwrap();
        assert_eq!(
            first_attempt.tally(&RaiElectionValue::CloseCutHash(first_hash)),
            1
        );
        assert_eq!(
            first_attempt.tally(&RaiElectionValue::CloseCutHash(updated_hash)),
            0
        );
        assert_eq!(
            second_attempt.tally(&RaiElectionValue::CloseCutHash(updated_hash)),
            1
        );
    }

    #[test]
    fn passively_started_close_record_publishes_local_first_vote() {
        let mut fixture = Fixture::two_members();
        let record_start = fixture.expired_epoch_time();

        {
            let mut state = fixture.close_state.write().unwrap();
            state.start_closing(0).unwrap();
            state.install_cut(0, VisibleSlots::new()).unwrap();
            state
                .record_cut_drain(0, std::iter::empty::<(RaiSlot, RaiClosedSlotState)>())
                .unwrap();
        }

        let record_hash = fixture
            .close_state
            .read()
            .unwrap()
            .current_close_record_hash(0)
            .unwrap();
        fixture
            .vote_processor
            .process(&RaiVote::new_first(
                &fixture.other_key,
                close_record_election_id(0, 0),
                RaiElectionValue::CloseRecordHash(record_hash),
            ))
            .unwrap();

        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_record_attempt_started(0, 0)
        );
        assert!(fixture.publisher.votes.lock().unwrap().is_empty());

        fixture.epoch_loop.tick_at(record_start);

        let votes = fixture.publisher.votes.lock().unwrap();
        let close_record_votes = votes
            .iter()
            .filter(|vote| vote.election_id == close_record_election_id(0, 0))
            .collect::<Vec<_>>();
        assert_eq!(close_record_votes.len(), 1);
        assert_eq!(close_record_votes[0].voter, fixture.local_key.public_key());
        assert_eq!(close_record_votes[0].kind, RaiVoteKind::First);
        assert_eq!(
            close_record_votes[0].value,
            RaiElectionValue::CloseRecordHash(record_hash)
        );
    }

    #[test]
    fn passively_started_close_record_timeout_waits_until_timeout_is_ready() {
        let mut fixture = Fixture::two_members();
        let record_start = fixture.expired_epoch_time();

        {
            let mut state = fixture.close_state.write().unwrap();
            state.start_closing(0).unwrap();
            state.install_cut(0, VisibleSlots::new()).unwrap();
            state
                .record_cut_drain(0, std::iter::empty::<(RaiSlot, RaiClosedSlotState)>())
                .unwrap();
        }
        fixture
            .vote_processor
            .process(&RaiVote::new_notarization(
                &fixture.other_key,
                close_record_election_id(0, 0),
                RaiElectionValue::Timeout,
            ))
            .unwrap();

        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_record_attempt_started(0, 0)
        );

        fixture.epoch_loop.tick_at(record_start);
        fixture
            .epoch_loop
            .tick_at(record_start + fixture.config.close_attempt_duration);

        let votes = fixture.publisher.votes.lock().unwrap();
        assert!(votes.iter().all(|vote| {
            vote.election_id != close_record_election_id(0, 0)
                || vote.kind != RaiVoteKind::Notarization
                || vote.value != RaiElectionValue::Timeout
        }));
    }

    #[test]
    fn epoch_timeout_starts_close_with_five_of_six_reports() {
        let keys: Vec<_> = (1..=6).map(PrivateKey::from).collect();
        let mut fixture = Fixture::with_committee(
            keys[0].clone(),
            keys[1].clone(),
            committee([
                (keys[0].public_key(), Amount::raw(100)),
                (keys[1].public_key(), Amount::raw(100)),
                (keys[2].public_key(), Amount::raw(100)),
                (keys[3].public_key(), Amount::raw(100)),
                (keys[4].public_key(), Amount::raw(100)),
                (keys[5].public_key(), Amount::raw(100)),
            ]),
        );
        fixture.mark_slot_visible();
        let now = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(now);
        assert!(
            !fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 0)
        );

        for key in keys.iter().skip(1).take(4) {
            fixture
                .pending_report_processor
                .process(&RaiPendingReport::new(key, 0, vec![fixture.slot]))
                .unwrap();
        }

        fixture.epoch_loop.tick_at(now + Duration::from_secs(1));

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.pending_report_count(0), 5);
        assert!(state.close_attempt_started(0, 0));
    }

    #[test]
    fn epoch_timeout_starts_close_with_four_of_five_reports() {
        let keys: Vec<_> = (1..=5).map(PrivateKey::from).collect();
        let mut fixture = Fixture::with_committee(
            keys[0].clone(),
            keys[1].clone(),
            committee([
                (keys[0].public_key(), Amount::raw(100)),
                (keys[1].public_key(), Amount::raw(100)),
                (keys[2].public_key(), Amount::raw(100)),
                (keys[3].public_key(), Amount::raw(100)),
                (keys[4].public_key(), Amount::raw(100)),
            ]),
        );
        fixture.mark_slot_visible();
        let now = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(now);
        assert!(
            !fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 0)
        );

        for key in keys.iter().skip(1).take(3) {
            fixture
                .pending_report_processor
                .process(&RaiPendingReport::new(key, 0, vec![fixture.slot]))
                .unwrap();
        }

        fixture.epoch_loop.tick_at(now + Duration::from_secs(1));

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.pending_report_count(0), 4);
        assert!(state.close_attempt_started(0, 0));
    }

    #[test]
    fn pending_report_truncates_visible_slots_to_payload_limit() {
        let mut fixture = Fixture::single_member();
        for index in 0..=RaiPendingReport::MAX_SLOTS {
            fixture
                .close_state
                .write()
                .unwrap()
                .mark_visible(0, RaiSlot::new(Account::from((index + 1) as u64), 1));
        }

        fixture.epoch_loop.tick_at(fixture.expired_epoch_time());

        let reports = fixture.publisher.reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].slots.len(), RaiPendingReport::MAX_SLOTS);
        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 0)
        );
    }

    #[test]
    fn pending_report_keeps_observed_nanospam_slot_count() {
        let mut fixture = Fixture::single_member();
        for index in 0..2850 {
            fixture
                .close_state
                .write()
                .unwrap()
                .mark_visible(0, RaiSlot::new(Account::from((index + 1) as u64), 1));
        }

        fixture.epoch_loop.tick_at(fixture.expired_epoch_time());

        let reports = fixture.publisher.reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].slots.len(), 2850);
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
        fixture.mark_slot_visible();
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
            state.closed_slot_state(0, &fixture.slot),
            Some(&RaiClosedSlotState::Finalized(BlockHash::from(9)))
        );
    }

    #[test]
    fn included_cut_times_out_unconfirmed_slots_and_advances_epoch() {
        let mut fixture = Fixture::single_member();
        fixture
            .active_elections
            .insert(fixture.slot_election_id())
            .unwrap();
        fixture.mark_slot_visible();
        let close_start = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(close_start);
        assert_eq!(
            fixture.close_state.read().unwrap().current_epoch_phase(),
            RaiEpochPhase::Closing
        );

        fixture
            .epoch_loop
            .tick_at(close_start + fixture.config.close_attempt_duration);

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.current_epoch(), 1);
        assert_eq!(state.epoch_phase(0), Some(RaiEpochPhase::Closed));
        assert_eq!(
            state.closed_slot_state(0, &fixture.slot),
            Some(&RaiClosedSlotState::Released)
        );
        assert!(fixture.publisher.votes.lock().unwrap().iter().any(|vote| {
            matches!(
                vote.election_id,
                RaiElectionId::Slot {
                    slot,
                    epoch: 0,
                } if slot == fixture.slot
            ) && vote.kind == RaiVoteKind::Notarization
                && vote.value == RaiElectionValue::Timeout
        }));
    }

    #[test]
    fn passive_epoch_advance_resets_epoch_timer() {
        let mut fixture = Fixture::single_member();
        let expired = fixture.expired_epoch_time();

        {
            let mut state = fixture.close_state.write().unwrap();
            state.start_closing(0).unwrap();
            state.install_cut(0, VisibleSlots::new()).unwrap();
            state
                .record_cut_drain(0, std::iter::empty::<(RaiSlot, RaiClosedSlotState)>())
                .unwrap();
            state.record_current_close_record_value(0).unwrap();
            state.advance_epoch(0).unwrap();
        }

        let observed_advance = expired + Duration::from_secs(1);
        fixture.epoch_loop.tick_at(observed_advance);

        {
            let state = fixture.close_state.read().unwrap();
            assert_eq!(state.current_epoch(), 1);
            assert_eq!(state.current_epoch_phase(), RaiEpochPhase::Open);
        }
        assert_eq!(fixture.epoch_loop.observed_epoch, 1);
        assert_eq!(fixture.epoch_loop.epoch_started_at, observed_advance);

        fixture
            .epoch_loop
            .tick_at(observed_advance + fixture.config.epoch_duration - Duration::from_millis(1));
        assert_eq!(
            fixture.close_state.read().unwrap().current_epoch_phase(),
            RaiEpochPhase::Open
        );
    }

    #[test]
    fn drained_cut_starts_close_record_and_waits_for_record_certificate() {
        let mut fixture = Fixture::two_members();
        let slot_election = fixture.slot_election_id();
        let block_value = RaiElectionValue::Block(BlockHash::from(9));
        fixture
            .active_elections
            .insert(slot_election.clone())
            .unwrap();
        fixture.epoch_loop.tick_at(fixture.expired_epoch_time());
        fixture
            .pending_report_processor
            .process(&RaiPendingReport::new(
                &fixture.other_key,
                0,
                vec![fixture.slot],
            ))
            .unwrap();
        let close_start = fixture.expired_epoch_time() + Duration::from_secs(1);
        fixture.epoch_loop.tick_at(close_start);
        let close_hash = fixture.close_state.read().unwrap().current_close_hash(0);
        fixture
            .vote_processor
            .process(&RaiVote::new_first(
                &fixture.other_key,
                close_cut_election_id(0, 0),
                RaiElectionValue::CloseCutHash(close_hash),
            ))
            .unwrap();
        fixture
            .epoch_loop
            .tick_at(close_start + Duration::from_secs(1));

        fixture
            .vote_processor
            .process(&RaiVote::new_final(
                &fixture.local_key,
                slot_election.clone(),
                block_value.clone(),
            ))
            .unwrap();
        fixture
            .vote_processor
            .process(&RaiVote::new_final(
                &fixture.other_key,
                slot_election,
                block_value,
            ))
            .unwrap();
        let record_start = close_start + Duration::from_secs(2);
        fixture.epoch_loop.tick_at(record_start);

        {
            let state = fixture.close_state.read().unwrap();
            assert!(state.close_record_attempt_started(0, 0));
            assert_eq!(state.current_epoch(), 0);
            assert_eq!(state.current_epoch_phase(), RaiEpochPhase::Closing);
        }

        let record_hash = fixture
            .close_state
            .read()
            .unwrap()
            .current_close_record_hash(0)
            .unwrap();
        fixture
            .vote_processor
            .process(&RaiVote::new_first(
                &fixture.other_key,
                close_record_election_id(0, 0),
                RaiElectionValue::CloseRecordHash(record_hash),
            ))
            .unwrap();
        fixture
            .epoch_loop
            .tick_at(record_start + Duration::from_secs(1));

        let state = fixture.close_state.read().unwrap();
        assert_eq!(state.current_epoch(), 1);
        assert_eq!(state.epoch_phase(0), Some(RaiEpochPhase::Closed));
        assert!(state.close_record_attempt_processed(0, 0));
    }

    #[test]
    fn timed_out_close_attempt_retries_with_next_attempt() {
        let keys: Vec<_> = (1..=5).map(PrivateKey::from).collect();
        let mut fixture = Fixture::with_committee(
            keys[0].clone(),
            keys[1].clone(),
            committee([
                (keys[0].public_key(), Amount::raw(100)),
                (keys[1].public_key(), Amount::raw(100)),
                (keys[2].public_key(), Amount::raw(100)),
                (keys[3].public_key(), Amount::raw(100)),
                (keys[4].public_key(), Amount::raw(100)),
            ]),
        );
        let first_slot = fixture.slot;
        let second_slot = RaiSlot::new(Account::from(2), 1);
        let (old_hash, current_hash) = {
            let mut close_state = fixture.close_state.write().unwrap();
            close_state.mark_visible(0, first_slot);
            let old_hash = close_state.record_current_close_value(0);
            close_state.mark_visible(0, second_slot);
            let current_hash = close_state.record_current_close_value(0);
            (old_hash, current_hash)
        };
        let close_start = fixture.expired_epoch_time();
        fixture.epoch_loop.tick_at(close_start);

        for key in keys.iter().skip(1).take(2) {
            fixture
                .vote_processor
                .process(&RaiVote::new_first(
                    key,
                    close_election_id(0, 0),
                    RaiElectionValue::CloseCutHash(current_hash),
                ))
                .unwrap();
        }
        fixture
            .vote_processor
            .process(&RaiVote::new_first(
                &keys[3],
                close_election_id(0, 0),
                RaiElectionValue::CloseCutHash(old_hash),
            ))
            .unwrap();
        let join_time = close_start + Duration::from_secs(1);
        fixture.epoch_loop.tick_at(join_time);
        fixture
            .vote_processor
            .process(&RaiVote::new_first(
                &keys[4],
                close_election_id(0, 0),
                RaiElectionValue::CloseCutHash(old_hash),
            ))
            .unwrap();

        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 0)
        );

        fixture
            .epoch_loop
            .tick_at(join_time + fixture.config.close_attempt_duration);
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

        for key in keys.iter().skip(1).take(3) {
            fixture
                .vote_processor
                .process(&RaiVote::new_notarization(
                    key,
                    close_election_id(0, 0),
                    RaiElectionValue::Timeout,
                ))
                .unwrap();
        }
        fixture
            .epoch_loop
            .tick_at(join_time + fixture.config.close_attempt_duration + Duration::from_secs(1));

        assert!(
            fixture
                .close_state
                .read()
                .unwrap()
                .close_attempt_started(0, 1)
        );
    }

    #[test]
    fn converged_close_attempt_carries_hash_to_next_attempt() {
        let keys: Vec<_> = (1..=5).map(PrivateKey::from).collect();
        let mut fixture = Fixture::with_committee(
            keys[0].clone(),
            keys[1].clone(),
            committee([
                (keys[0].public_key(), Amount::raw(100)),
                (keys[1].public_key(), Amount::raw(100)),
                (keys[2].public_key(), Amount::raw(100)),
                (keys[3].public_key(), Amount::raw(100)),
                (keys[4].public_key(), Amount::raw(100)),
            ]),
        );
        fixture.mark_slot_visible();
        let close_start = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(close_start);
        for key in keys.iter().skip(1).take(3) {
            fixture
                .pending_report_processor
                .process(&RaiPendingReport::new(key, 0, vec![fixture.slot]))
                .unwrap();
        }
        let attempt_start = close_start + Duration::from_secs(1);
        fixture.epoch_loop.tick_at(attempt_start);
        let close_hash = fixture.close_state.read().unwrap().current_close_hash(0);
        for key in keys.iter().skip(1).take(3) {
            fixture
                .vote_processor
                .process(&RaiVote::new_first(
                    key,
                    close_election_id(0, 0),
                    RaiElectionValue::CloseCutHash(close_hash),
                ))
                .unwrap();
        }

        fixture
            .epoch_loop
            .tick_at(attempt_start + Duration::from_secs(1));

        let state = fixture.close_state.read().unwrap();
        assert!(state.close_attempt_processed(0, 0));
        assert!(state.close_attempt_started(0, 1));
        assert!(state.cut_set(0).is_none());
        drop(state);

        let votes = fixture.publisher.votes.lock().unwrap();
        assert!(votes.iter().any(|vote| {
            vote.election_id == close_election_id(0, 1)
                && vote.kind == RaiVoteKind::First
                && vote.value == RaiElectionValue::CloseCutHash(close_hash)
        }));
        drop(votes);

        fixture
            .vote_processor
            .process(&RaiVote::new_first(
                &keys[4],
                close_election_id(0, 0),
                RaiElectionValue::CloseCutHash(close_hash),
            ))
            .unwrap();
        fixture
            .epoch_loop
            .tick_at(attempt_start + Duration::from_secs(2));

        assert!(fixture.close_state.read().unwrap().cut_set(0).is_some());
    }

    #[test]
    fn decided_close_cut_ignores_late_convergence_from_another_round() {
        let keys: Vec<_> = (1..=5).map(PrivateKey::from).collect();
        let mut fixture = Fixture::with_committee(
            keys[0].clone(),
            keys[1].clone(),
            committee([
                (keys[0].public_key(), Amount::raw(100)),
                (keys[1].public_key(), Amount::raw(100)),
                (keys[2].public_key(), Amount::raw(100)),
                (keys[3].public_key(), Amount::raw(100)),
                (keys[4].public_key(), Amount::raw(100)),
            ]),
        );
        fixture.mark_slot_visible();
        let close_start = fixture.expired_epoch_time();

        fixture.epoch_loop.tick_at(close_start);
        for key in keys.iter().skip(1).take(3) {
            fixture
                .pending_report_processor
                .process(&RaiPendingReport::new(key, 0, vec![fixture.slot]))
                .unwrap();
        }
        let attempt_start = close_start + Duration::from_secs(1);
        fixture.epoch_loop.tick_at(attempt_start);
        let close_hash = fixture.close_state.read().unwrap().current_close_hash(0);
        for key in keys.iter().skip(1).take(3) {
            fixture
                .vote_processor
                .process(&RaiVote::new_first(
                    key,
                    close_election_id(0, 0),
                    RaiElectionValue::CloseCutHash(close_hash),
                ))
                .unwrap();
        }

        // Model a certificate from another round arriving before this
        // converged outcome is consumed.
        fixture
            .close_state
            .write()
            .unwrap()
            .install_cut(0, [fixture.slot].into_iter().collect())
            .unwrap();
        fixture
            .epoch_loop
            .tick_at(attempt_start + Duration::from_secs(1));

        let state = fixture.close_state.read().unwrap();
        assert!(!state.close_attempt_started(0, 1));
        assert!(state.cut_set(0).is_some());
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
            let rep_tracker = Arc::new(RepresentativeTracker::default());
            let vote_processor = Arc::new(RaiVoteProcessor::with_committee_provider(
                active_elections.clone(),
                close_state.clone(),
                rep_tracker.clone(),
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
            let wallet_reps = local_wallet_reps(local_key.clone(), rep_tracker);
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
                wallet_reps,
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

        fn mark_slot_visible(&self) {
            self.close_state.write().unwrap().mark_visible(0, self.slot);
        }
    }

    fn local_wallet_reps(
        local_key: PrivateKey,
        rep_tracker: Arc<RepresentativeTracker>,
    ) -> Arc<Mutex<WalletRepresentatives>> {
        let wallets = Arc::new(Wallets::new_null());
        let wallet_id = WalletId::ZERO;
        wallets.create(wallet_id);
        wallets
            .insert_adhoc2(&wallet_id, &local_key.raw_key(), false)
            .unwrap();

        let rep_weights = Arc::new(RepWeightCache::default());
        rep_weights.put(local_key.public_key(), Amount::raw(100));
        let mut wallet_reps =
            WalletRepresentatives::new(true, Amount::ZERO, rep_weights, wallets, rep_tracker);
        wallet_reps.compute_reps();

        Arc::new(Mutex::new(wallet_reps))
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
