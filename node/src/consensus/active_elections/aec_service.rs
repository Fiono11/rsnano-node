use std::{collections::HashMap, sync::RwLock, time::Duration};

#[cfg(feature = "rai_protocol")]
use std::collections::{HashSet, VecDeque};

#[cfg(feature = "rai_protocol")]
use rsnano_ledger::AnySet;
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Account, Amount, Block, BlockHash, PublicKey, QualifiedRoot, SavedBlock, VoteError,
};
#[cfg(feature = "rai_protocol")]
use rsnano_utils::{CancellationToken, ticker::Tickable};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};

#[cfg(feature = "rai_protocol")]
use super::RaiCloseElectionSpec;
use super::{
    ActiveElectionsConfig, ActiveElectionsContainer, ActiveElectionsInfo, AecCooldownReason,
    AecFact, AecInsertError, AecInsertRequest, ApplyVoteArgs,
};
use crate::consensus::{
    ElectionCandidateSource,
    election::{ConfirmedElection, Election, ElectionBehavior, ElectionState},
};

pub struct AecService {
    aec: RwLock<ActiveElectionsContainer>,
    clock: SteadyClock,
}

#[cfg(feature = "rai_protocol")]
pub struct RaiEpochTicker {
    aec: std::sync::Arc<AecService>,
    clock: std::sync::Arc<SteadyClock>,
    wallet_reps: std::sync::Arc<std::sync::Mutex<crate::wallets::WalletRepresentatives>>,
    ledger: std::sync::Arc<rsnano_ledger::Ledger>,
    epoch_duration: Duration,
    flooder: crate::transport::MessageFlooder,
    known_reports: HashSet<Vec<u8>>,
    report_rebroadcast_queue: VecDeque<crate::consensus::rai::RaiReport>,
    local_key: Option<rsnano_types::PrivateKey>,
    last_report_request: Option<Timestamp>,
    last_close_vote_request: Option<Timestamp>,
    last_slot_vote_request: Option<Timestamp>,
    request_sequence: u64,
}

#[cfg(feature = "rai_protocol")]
impl RaiEpochTicker {
    pub fn new(
        aec: std::sync::Arc<AecService>,
        clock: std::sync::Arc<SteadyClock>,
        wallet_reps: std::sync::Arc<std::sync::Mutex<crate::wallets::WalletRepresentatives>>,
        ledger: std::sync::Arc<rsnano_ledger::Ledger>,
        epoch_duration: Duration,
        flooder: crate::transport::MessageFlooder,
        _vote_history: std::sync::Arc<crate::consensus::LocalVoteHistory>,
    ) -> Self {
        Self {
            aec,
            clock,
            wallet_reps,
            ledger,
            epoch_duration,
            flooder,
            known_reports: Default::default(),
            report_rebroadcast_queue: Default::default(),
            local_key: None,
            last_report_request: None,
            last_close_vote_request: None,
            last_slot_vote_request: None,
            request_sequence: 0,
        }
    }
}

#[cfg(feature = "rai_protocol")]
impl Tickable for RaiEpochTicker {
    fn tick(&mut self, _cancel_token: &CancellationToken) {
        if self.local_key.is_none() {
            let mut keys = Vec::new();
            self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
            self.local_key = keys.into_iter().next();
        }
        let Some(local_key) = self.local_key.as_ref() else {
            // Reports are committee votes and must be signed by a voting
            // representative. A node-id signature has no committee weight.
            return;
        };
        let now = self.clock.now();
        self.aec.rai_tick(now, local_key, self.epoch_duration);
        let closing = self.aec.rai_epoch_status().0.closing;
        if let Some(closing) = closing {
            self.aec.rai_progress_close(
                self.ledger.rai_preceding_frontiers(closing.epoch),
                &self.ledger,
                now,
            );
            if closing.phase == crate::consensus::rai::RaiClosingPhase::Draining {
                // A replica may learn the winning cut after the corresponding
                // live election was cleaned up (or before it ever observed
                // that election). Recreate every missing obligation locally;
                // the sequenced repair requests below can only solicit votes
                // for elections which exist and have the winning cut context.
                for root in self.aec.rai_missing_drain_elections(closing.epoch) {
                    let any = self.ledger.any();
                    let Some(hash) = any.block_successor_by_qualified_root(&root) else {
                        continue;
                    };
                    let Some(block) = any.get_block(&hash) else {
                        continue;
                    };
                    let _ = self.aec.insert_drain_election(block, closing.epoch, now);
                }
            }
            // A report quorum is only enough to propose a cut.  Peers may have
            // reached quorum from different subsets and therefore be voting on
            // different cut hashes.  Keep repairing the report set until the
            // cut election itself has a terminal certificate, so every peer can
            // validate the winning candidate preimage and apply its votes.
            if matches!(
                closing.phase,
                crate::consensus::rai::RaiClosingPhase::CollectingReports
                    | crate::consensus::rai::RaiClosingPhase::ElectingCut
            ) && self
                .last_report_request
                .is_none_or(|last| last.elapsed(now) >= Duration::from_millis(500))
            {
                self.request_sequence = self.request_sequence.wrapping_add(1);
                self.flooder.flood_prs_and_some_non_prs(
                    &rsnano_messages::Message::RaiReportRequest(
                        rsnano_messages::RaiReportRequest {
                            epoch: closing.epoch,
                            sequence: self.request_sequence,
                        },
                    ),
                    rsnano_network::TrafficType::Generic,
                    1.0,
                );
                self.last_report_request = Some(now);
            }
            // Repair the active round. After a timeout certificate the root
            // changes with the round number; continuing to request round zero
            // strands replicas with incomplete First evidence forever.
            let close_root = self.aec.rai_current_close_root();
            const CLOSE_REPAIR_INTERVAL: Duration = Duration::from_secs(2);
            if let Some(root) = close_root
                && self
                    .last_close_vote_request
                    .is_none_or(|last| last.elapsed(now) >= CLOSE_REPAIR_INTERVAL)
            {
                self.request_sequence = self.request_sequence.wrapping_add(1);
                self.flooder.flood_prs_and_some_non_prs(
                    &rsnano_messages::Message::RaiVoteRequest(rsnano_messages::RaiVoteRequest {
                        sequence: self.request_sequence,
                        epoch: closing.epoch.number(),
                        hash: BlockHash::ZERO,
                        root,
                        close_version: None,
                    }),
                    rsnano_network::TrafficType::ConfirmationRequests,
                    // Close repair is certificate retrieval, not epidemic
                    // gossip. Before representative tracking converges, the
                    // ordinary random fanout can repeatedly miss committee
                    // signers whose leaves are required for progress.
                    8.0,
                );
                self.last_close_vote_request = Some(now);
            }
            if closing.phase == crate::consensus::rai::RaiClosingPhase::Draining
                && self
                    .last_slot_vote_request
                    .is_none_or(|last| last.elapsed(now) >= Duration::from_secs(2))
            {
                for request in self.aec.rai_pending_slot_requests(closing.epoch) {
                    self.request_sequence = self.request_sequence.wrapping_add(1);
                    self.flooder.flood_prs_and_some_non_prs(
                        &rsnano_messages::Message::RaiVoteRequest(
                            rsnano_messages::RaiVoteRequest {
                                sequence: self.request_sequence,
                                epoch: closing.epoch.number(),
                                hash: request.0,
                                root: request.1,
                                close_version: None,
                            },
                        ),
                        rsnano_network::TrafficType::ConfirmationRequests,
                        8.0,
                    );
                }
                self.last_slot_vote_request = Some(now);
            }
        }
        // Reports use the same epidemic dissemination model as legacy votes:
        // relay each newly learned immutable object once to all known PRs and
        // a bounded random fanout of other peers. Receiving peers repeat this
        // step, while their report identity set prevents gossip loops.
        const MAX_REPORT_QUEUE: usize = 16 * 1024;
        let available = MAX_REPORT_QUEUE.saturating_sub(self.report_rebroadcast_queue.len());
        self.report_rebroadcast_queue.extend(
            newly_seen_reports(&mut self.known_reports, self.aec.rai_reports())
                .into_iter()
                .take(available),
        );
        while !self.report_rebroadcast_queue.is_empty()
            && self
                .flooder
                .check_capacity(rsnano_network::TrafficType::Generic, 1.0)
        {
            let report = self.report_rebroadcast_queue.pop_front().unwrap();
            tracing::warn!(?report, "RAI_CLOSE_TRACE report send epidemic");
            self.flooder.flood_prs_and_some_non_prs(
                &rsnano_messages::Message::RaiReport(report.clone().into()),
                rsnano_network::TrafficType::Generic,
                1.0,
            );
        }
    }
}

#[cfg(feature = "rai_protocol")]
fn newly_seen_reports(
    known: &mut HashSet<Vec<u8>>,
    reports: Vec<crate::consensus::rai::RaiReport>,
) -> Vec<crate::consensus::rai::RaiReport> {
    reports
        .into_iter()
        .filter(|report| {
            let mut identity = report.reporter.as_bytes().to_vec();
            identity.extend_from_slice(&report.signing_bytes());
            known.insert(identity)
        })
        .collect()
}

impl AecService {
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::new(config, base_latency)),
            clock: SteadyClock::default(),
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_with_rai_committee(
        config: ActiveElectionsConfig,
        base_latency: Duration,
        genesis_committee: std::sync::Arc<rsnano_ledger::RepWeights>,
        genesis_governing_hash: BlockHash,
        ledger: std::sync::Arc<rsnano_ledger::Ledger>,
    ) -> Self {
        let clock = SteadyClock::default();
        let mut aec = ActiveElectionsContainer::new_with_rai_committee(
            config,
            base_latency,
            genesis_committee,
            genesis_governing_hash,
        );
        aec.set_rai_ledger(ledger);
        // Epoch zero begins when this node initializes RAI. Leaving the
        // manager at Timestamp::default() makes the first ticker invocation
        // close epoch zero immediately, before nanospam can publish work.
        aec.rai_set_open_started_at(clock.now());
        Self {
            aec: RwLock::new(aec),
            clock,
        }
    }

    pub fn new_null() -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::default()),
            clock: SteadyClock::new_null(),
        }
    }

    // --- Read forwarding ---

    pub fn check_vacancy<T>(&self, source: &T) -> bool
    where
        T: ElectionCandidateSource,
    {
        self.aec.read().unwrap().check_vacancy(source)
    }

    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<Election> {
        self.aec.read().unwrap().election_for_root(root).cloned()
    }

    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<Election> {
        self.aec
            .read()
            .unwrap()
            .election_for_block(block_hash)
            .cloned()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_vote_context(
        &self,
        block_hash: &BlockHash,
    ) -> Option<(rsnano_types::RaiVoteMetadata, bool)> {
        self.aec.read().unwrap().rai_vote_context(block_hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_close_vote_context_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<rsnano_types::RaiVoteMetadata> {
        self.aec
            .read()
            .unwrap()
            .rai_close_vote_context_for_root(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_active_close_vote_target_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<rsnano_ledger::RaiFinalizedVoteTarget> {
        self.aec
            .read()
            .unwrap()
            .rai_active_close_vote_target_for_root(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_slot_vote_context_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Option<rsnano_types::RaiVoteMetadata> {
        self.aec
            .read()
            .unwrap()
            .rai_slot_vote_context_for_root(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_terminal_notarized_target_for_root(
        &self,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Option<(BlockHash, rsnano_types::RaiVoteMetadata)> {
        self.aec
            .read()
            .unwrap()
            .rai_terminal_notarized_target_for_root(root, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_active_slot_vote_target_for_root(
        &self,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Option<rsnano_ledger::RaiFinalizedVoteTarget> {
        self.aec
            .read()
            .unwrap()
            .rai_active_slot_vote_target_for_root(root, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_votes_for_root(
        &self,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<rsnano_types::Vote> {
        self.aec.read().unwrap().rai_votes_for_root(root, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_blocks_for_request(
        &self,
        hash: BlockHash,
        root: rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<Block> {
        self.aec
            .read()
            .unwrap()
            .rai_blocks_for_request(hash, root, epoch)
    }

    pub fn max_len(&self) -> usize {
        self.aec.read().unwrap().max_len()
    }

    pub fn len(&self) -> usize {
        self.aec.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.aec.read().unwrap().is_empty()
    }

    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.aec.read().unwrap().is_active_root(root)
    }

    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.aec.read().unwrap().is_active_hash(block_hash)
    }

    pub fn was_recently_confirmed(&self, block_hash: &BlockHash) -> bool {
        self.aec.read().unwrap().was_recently_confirmed(block_hash)
    }

    pub fn count_by_behavior(&self, behavior: ElectionBehavior) -> usize {
        self.aec.read().unwrap().count_by_behavior(behavior)
    }

    pub fn vacancy(&self) -> i64 {
        self.aec.read().unwrap().vacancy()
    }

    pub fn info(&self) -> ActiveElectionsInfo {
        let now = self.clock.now();
        self.aec.read().unwrap().info(now)
    }

    pub fn round_robin<F, T>(&self, f: F) -> T
    where
        F: FnOnce(&mut dyn Iterator<Item = &Election>) -> T,
    {
        let guard = self.aec.read().unwrap();
        f(&mut guard.iter_round_robin())
    }

    // --- Write forwarding ---

    pub fn set_observer(&self, observer: Sender<AecFact>) {
        self.aec.write().unwrap().set_observer(observer)
    }

    pub fn insert(&self, request: AecInsertRequest, now: Timestamp) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert(request, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_cut(
        &self,
        spec: RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert_close_cut(spec, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_record(
        &self,
        spec: RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert_close_record(spec, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_record_versions(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<crate::consensus::rai::RaiCloseRecord> {
        self.aec.read().unwrap().rai_close_record_versions(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_record_versions_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Vec<crate::consensus::rai::RaiCloseRecord> {
        self.aec
            .read()
            .unwrap()
            .rai_close_record_versions_for_root(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_cut_versions_for_root(
        &self,
        root: &rsnano_types::Root,
    ) -> Vec<crate::consensus::rai::RaiCloseCut> {
        self.aec
            .read()
            .unwrap()
            .rai_close_cut_versions_for_root(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_cut_versions(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<crate::consensus::rai::RaiCloseCut> {
        self.aec.read().unwrap().rai_close_cut_versions(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_votes_for_epoch(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<rsnano_types::Vote> {
        self.aec.read().unwrap().rai_close_votes_for_epoch(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn reconcile_rai_close_cut(
        &self,
        cut: crate::consensus::rai::RaiCloseCut,
        root: rsnano_types::Root,
    ) -> bool {
        self.aec
            .write()
            .unwrap()
            .reconcile_rai_close_cut(cut, root, self.clock.now())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn reconcile_rai_close_record(
        &self,
        record: crate::consensus::rai::RaiCloseRecord,
        root: rsnano_types::Root,
    ) -> bool {
        self.aec
            .write()
            .unwrap()
            .reconcile_rai_close_record(record, root, self.clock.now())
    }

    pub fn try_add_fork(&self, fork: &Block, fork_tally: Amount) -> bool {
        self.aec.write().unwrap().try_add_fork(fork, fork_tally)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn published_block_available(&self, block: Block) {
        self.aec.write().unwrap().published_block_available(block)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn admit_candidate(
        &self,
        slot: crate::consensus::election::RaiSlotId,
        candidate: BlockHash,
    ) -> Result<(), super::CandidateError> {
        self.aec.write().unwrap().admit_candidate(slot, candidate)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn has_published_block(&self, hash: &BlockHash) -> bool {
        self.aec.read().unwrap().known_block(hash).is_some()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn published_blocks_at_root(&self, root: &QualifiedRoot) -> Vec<BlockHash> {
        self.aec
            .read()
            .unwrap()
            .candidate_hashes_at_root(root)
            .copied()
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn slot_contains_candidate(
        &self,
        slot: &crate::consensus::election::RaiSlotId,
        hash: &BlockHash,
    ) -> bool {
        self.aec.read().unwrap().slot_contains_candidate(slot, hash)
    }

    pub fn apply_vote<'a>(
        &self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        self.aec.write().unwrap().apply_vote(args)
    }

    pub fn transition_time(&self, now: Timestamp) {
        self.aec.write().unwrap().transition_time(now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_tick(
        &self,
        now: Timestamp,
        local_key: &rsnano_types::PrivateKey,
        epoch_duration: Duration,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.aec
            .write()
            .unwrap()
            .rai_tick(now, local_key, epoch_duration)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_report_received(&self, report: crate::consensus::rai::RaiReport) {
        tracing::warn!(?report, "RAI_CLOSE_TRACE report receive");
        let now = self.clock.now();
        self.aec.write().unwrap().rai_report_received(
            report,
            &rsnano_types::PrivateKey::from(0),
            Duration::from_secs(1),
            now,
        );
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_reports(&self) -> Vec<crate::consensus::rai::RaiReport> {
        self.aec.read().unwrap().rai_reports()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_current_close_root(&self) -> Option<rsnano_types::Root> {
        self.aec.read().unwrap().rai_current_close_root()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_reports_for_epoch(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.rai_reports()
            .into_iter()
            .filter(|report| report.epoch == epoch)
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_finalized_vote_target(
        &self,
        ledger: &rsnano_ledger::Ledger,
        hash: &BlockHash,
        root: &rsnano_types::Root,
        requested_epoch: rsnano_types::RaiEpoch,
    ) -> Option<rsnano_ledger::RaiFinalizedVoteTarget> {
        let aec = self.aec.read().unwrap();
        if let Some(target) = aec.rai_finalized_close_vote_target(root)
            && target.metadata.epoch == requested_epoch
        {
            return Some(target);
        }
        if let Some(target) = aec.rai_certificate_finalized_vote_target(hash, root, requested_epoch)
        {
            return Some(target);
        }
        drop(aec);
        let target = ledger.rai_finalized_vote_target(hash, root)?;
        let epoch = target.metadata.epoch;
        let aec = self.aec.read().unwrap();
        if epoch != requested_epoch
            || !aec.rai_has_governing_context(epoch)
            || !aec.rai_election_vote_enabled(&target.election_id)
        {
            return None;
        }
        Some(target)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn rai_has_active_request_target(
        &self,
        hash: &BlockHash,
        root: &rsnano_types::Root,
        epoch: rsnano_types::RaiEpoch,
    ) -> bool {
        self.aec
            .read()
            .unwrap()
            .rai_has_active_request_target(hash, root, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_pending_slot_requests(
        &self,
        epoch: rsnano_types::RaiEpoch,
    ) -> Vec<(BlockHash, rsnano_types::Root)> {
        self.aec.read().unwrap().rai_pending_slot_requests(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_progress_close(
        &self,
        frontiers: crate::consensus::rai::RaiFrontierMap,
        ledger: &rsnano_ledger::Ledger,
        now: Timestamp,
    ) {
        self.aec
            .write()
            .unwrap()
            .rai_progress_close(frontiers, ledger, now);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_missing_drain_elections(&self, epoch: rsnano_types::RaiEpoch) -> Vec<QualifiedRoot> {
        self.aec.read().unwrap().rai_missing_drain_elections(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_drain_election(
        &self,
        block: SavedBlock,
        epoch: rsnano_types::RaiEpoch,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.aec
            .write()
            .unwrap()
            .insert_drain_election(block, epoch, now)
    }

    pub fn transition_active(&self, block_hash: &BlockHash) -> bool {
        self.aec.write().unwrap().transition_active(block_hash)
    }

    pub fn refill<T>(&self, source: &mut T, now: Timestamp)
    where
        T: ElectionCandidateSource,
    {
        self.aec.write().unwrap().refill(source, now);
    }

    pub fn remove_votes<'a>(
        &self,
        root: &QualifiedRoot,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        self.aec.write().unwrap().remove_votes(root, voters)
    }

    pub fn erase(&self, root: &QualifiedRoot) -> bool {
        self.aec.write().unwrap().erase(root)
    }

    pub fn confirm_dependent_elections(
        &self,
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>)>,
        now: Timestamp,
    ) {
        self.aec
            .write()
            .unwrap()
            .confirm_dependent_elections(confirmed, now)
    }

    pub fn remove_recently_confirmed(&self, block_hash: &BlockHash) {
        self.aec
            .write()
            .unwrap()
            .remove_recently_confirmed(block_hash)
    }

    pub fn set_cooldown(&self, cool_down: bool, reason: AecCooldownReason) {
        self.aec.write().unwrap().set_cooldown(cool_down, reason)
    }

    pub fn cancel(&self, root: &QualifiedRoot) {
        self.aec.write().unwrap().cancel(root)
    }

    pub fn cancel_all(&self) {
        self.aec.write().unwrap().cancel_all()
    }

    pub fn clear_recently_confirmed(&self) {
        self.aec.write().unwrap().clear_recently_confirmed()
    }

    pub fn stop(&self) {
        self.aec.write().unwrap().stop()
    }

    pub fn force_confirm(&self, block_hash: &BlockHash, now: Timestamp) {
        self.aec.write().unwrap().force_confirm(block_hash, now)
    }

    pub fn simulate_event(&self, event: AecFact) {
        self.aec.read().unwrap().simulate_event(event)
    }

    pub fn snapshot(&self) -> AecSnapshot {
        let now = self.clock.now();
        self.aec.read().unwrap().snapshot(now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_epoch_status(
        &self,
    ) -> (
        crate::consensus::rai::RaiEpochState,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, BlockHash>,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, BlockHash>,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, (usize, usize)>,
    ) {
        let aec = self.aec.read().unwrap();
        let state = *aec.rai_epoch_state();
        let mut hashes = std::collections::BTreeMap::new();
        if let Some(last) = state.closed_through {
            for number in 0..=last.number() {
                let epoch = rsnano_types::RaiEpoch::new(number);
                if let Some(hash) = aec.rai_installed_close_hash(epoch) {
                    hashes.insert(epoch, hash);
                }
            }
        }
        let cut_hashes = aec.rai_decided_cut_hashes().clone();
        let drains = aec
            .rai_happy_path_drains()
            .iter()
            .map(|(epoch, drain)| {
                (
                    *epoch,
                    (
                        drain.obligations.len(),
                        drain.finalized.len() + drain.selected.len() + drain.released.len(),
                    ),
                )
            })
            .collect();
        (state, hashes, cut_hashes, drains)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_genesis_committee(&self) -> std::sync::Arc<rsnano_ledger::RepWeights> {
        self.aec.read().unwrap().rai_genesis_committee()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_close_election_durations(
        &self,
    ) -> (
        std::collections::BTreeMap<rsnano_types::RaiEpoch, Duration>,
        std::collections::BTreeMap<rsnano_types::RaiEpoch, Duration>,
    ) {
        let aec = self.aec.read().unwrap();
        let (cut, record) = aec.rai_close_election_durations();
        (cut.clone(), record.clone())
    }
}

impl StatsSource for AecService {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.aec.read().unwrap().collect_stats(result)
    }
}

impl ContainerInfoProvider for AecService {
    fn container_info(&self) -> ContainerInfo {
        self.aec.read().unwrap().container_info()
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use std::{sync::Arc, time::Duration};

    use rsnano_ledger::RepWeights;
    use rsnano_types::{Amount, BlockHash, PrivateKey, RaiEpoch};

    use super::*;

    #[test]
    fn each_distinct_report_is_relayed_once() {
        use crate::consensus::rai::RaiReport;

        let first = RaiReport::new(&PrivateKey::from(1), RaiEpoch::ZERO, []);
        let second = RaiReport::new(&PrivateKey::from(2), RaiEpoch::ZERO, []);
        let mut known = HashSet::new();

        assert_eq!(
            newly_seen_reports(&mut known, vec![first.clone(), second.clone()]),
            vec![first.clone(), second.clone()]
        );
        assert!(newly_seen_reports(&mut known, vec![first, second]).is_empty());
    }

    #[test]
    fn live_tick_opens_epoch_one_at_the_deadline() {
        let key = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(key.public_key(), Amount::raw(1));
        let service = AecService::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_millis(25),
            Arc::new(weights),
            BlockHash::ZERO,
            Arc::new(rsnano_ledger::Ledger::new_null()),
        );
        let duration = Duration::from_secs(30);
        let start = service.clock.now();

        service.rai_tick(start, &key, duration);
        assert_eq!(service.rai_epoch_status().0.open_epoch, RaiEpoch::ZERO);

        service.rai_tick(start + duration, &key, duration);
        let state = service.rai_epoch_status().0;
        assert_eq!(state.open_epoch, RaiEpoch::new(1));
        assert_eq!(state.closing.unwrap().epoch, RaiEpoch::ZERO);
    }

    #[test]
    fn received_report_quorum_starts_the_live_cut_election() {
        use crate::consensus::rai::{RaiClosingPhase, RaiReport, rai_close_cut_root};

        let keys = (1..=4).map(PrivateKey::from).collect::<Vec<_>>();
        let mut weights = RepWeights::default();
        for key in &keys {
            weights.put(key.public_key(), Amount::raw(1));
        }
        let service = AecService::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_millis(25),
            Arc::new(weights),
            BlockHash::ZERO,
            Arc::new(rsnano_ledger::Ledger::new_null()),
        );
        let duration = Duration::from_secs(30);
        let deadline = service.clock.now() + duration;

        let reports = service.rai_tick(deadline, &keys[0], duration);
        assert_eq!(reports.len(), 1);
        assert_eq!(
            service.rai_epoch_status().0.closing.unwrap().phase,
            RaiClosingPhase::CollectingReports
        );
        assert!(!service.is_active_root(&rai_close_cut_root(RaiEpoch::ZERO, 0)));

        service.rai_report_received(RaiReport::new(&keys[1], RaiEpoch::ZERO, []));
        service.rai_report_received(RaiReport::new(&keys[2], RaiEpoch::ZERO, []));
        assert_eq!(service.rai_reports().len(), 3);
        assert_eq!(
            service.rai_epoch_status().0.closing.unwrap().phase,
            RaiClosingPhase::CollectingReports
        );
        service.rai_report_received(RaiReport::new(&keys[3], RaiEpoch::ZERO, []));
        service.rai_tick(deadline + Duration::from_millis(1), &keys[0], duration);

        assert_eq!(
            service.rai_epoch_status().0.closing.unwrap().phase,
            RaiClosingPhase::ElectingCut
        );
        assert!(service.is_active_root(&rai_close_cut_root(RaiEpoch::ZERO, 0)));
    }
}

#[derive(Default)]
pub struct AecSnapshot {
    pub buckets: Vec<BucketSnapshot>,
}

pub struct BucketSnapshot {
    pub bucket_index: usize,
    pub election_count: usize,
    pub elections: Vec<ElectionSnapshot>,
}

pub struct ElectionSnapshot {
    pub winner_hash: BlockHash,
    pub non_final_tally: Amount,
    pub final_tally: Amount,
    pub root: QualifiedRoot,
    pub account: Account,
    pub state: ElectionState,
    pub candidate_blocks: Vec<BlockHash>,
    pub is_final: bool,
    pub elapsed: Duration,
}
