use std::{cmp::max, collections::HashMap, time::Duration};

use strum::EnumCount;

use rsnano_ledger::RepWeights;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Amount, Block, BlockHash, PublicKey, QualifiedRoot, SavedBlock, TimePriority, VoteError,
};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};

use crate::{
    consensus::{
        AecSnapshot, ElectionCandidateSource,
        election::{
            AddForkResult, ConfirmationType, ConfirmedElection, Election, ElectionBehavior,
        },
        election_schedulers::priority::bucket_count,
        filtered_vote::FilteredVote,
    },
    representatives::QuorumSnapshot,
};

use super::{
    ActiveElectionsConfig, ActiveElectionsInfo, AecFact, AecInsertError, AecInsertRequest, Entry,
    RootContainer,
    apply_vote_helper::ApplyVoteHelper,
    cooldown_controller::{AecCooldownReason, CooldownController, CooldownResult},
    recently_confirmed_cache::RecentlyConfirmedCache,
    stats::AecStats,
};

pub(crate) struct ActiveElectionsContainer {
    roots: RootContainer,
    observer: Option<Sender<AecFact>>,
    stopped: bool,
    count_by_behavior: [usize; ElectionBehavior::COUNT],
    base_latency: Duration,
    recently_confirmed: RecentlyConfirmedCache,
    cooldown: CooldownController,
    max_elections: usize,
    max_elections_per_bucket: usize,
    stats: AecStats,
    #[cfg(feature = "rai_protocol")]
    rai_epoch_manager: crate::consensus::rai::RaiEpochManager,
    #[cfg(feature = "rai_protocol")]
    rai_visible_obligations: std::collections::BTreeMap<
        rsnano_types::RaiEpoch,
        std::collections::BTreeSet<QualifiedRoot>,
    >,
    #[cfg(feature = "rai_protocol")]
    rai_terminal_slots: std::collections::BTreeMap<
        (rsnano_types::RaiEpoch, QualifiedRoot),
        (
            crate::consensus::rai::RaiElectionVoteState,
            rsnano_types::Account,
            Option<rsnano_types::ConfirmationHeightInfo>,
        ),
    >,
}

impl ActiveElectionsContainer {
    #[cfg(feature = "rai_protocol")]
    pub fn rai_tick(
        &mut self,
        now: Timestamp,
        local_key: &rsnano_types::PrivateKey,
        epoch_duration: Duration,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        self.process_rai_event(
            crate::consensus::rai::RaiEpochEvent::Tick(now),
            local_key,
            epoch_duration,
            now,
        )
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_report_received(
        &mut self,
        report: crate::consensus::rai::RaiReport,
        local_key: &rsnano_types::PrivateKey,
        epoch_duration: Duration,
        now: Timestamp,
    ) {
        self.process_rai_event(
            crate::consensus::rai::RaiEpochEvent::ReportReceived(report),
            local_key,
            epoch_duration,
            now,
        );
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_progress_close(
        &mut self,
        _frontiers: crate::consensus::rai::RaiFrontierMap,
        now: Timestamp,
    ) {
        use crate::consensus::rai::{RaiCloseElectionId, RaiCloseKind, RaiClosingPhase};

        let Some(closing) = self.rai_epoch_manager.closing_epoch() else {
            return;
        };
        if closing.phase == RaiClosingPhase::ElectingCut {
            let round = self
                .rai_epoch_manager
                .close_cut_round(closing.epoch)
                .unwrap_or(0);
            if let Some(hash) = self.rai_epoch_manager.refresh_close_cut_candidate(
                closing.epoch,
                round,
                std::iter::empty(),
            ) {
                let root = crate::consensus::rai::rai_close_cut_root(closing.epoch, round);
                self.roots.add_rai_hash_candidate(&root, hash);
            }
            return;
        }
        if closing.phase != RaiClosingPhase::Draining {
            return;
        }
        // Start from the certified cut, not each replica's asynchronously
        // advancing ledger view. Exact terminal slot frontiers are merged
        // below as their certificates settle the cut obligations.
        self.rai_epoch_manager
            .initialize_drain_frontiers(closing.epoch, []);
        let obligations = self
            .rai_epoch_manager
            .obligations_to_drain(closing.epoch)
            .cloned()
            .unwrap_or_default();
        for root in obligations {
            if let Some((evidence, account, confirmed)) =
                self.rai_terminal_slots.get(&(closing.epoch, root.clone()))
            {
                let segment = confirmed
                    .as_ref()
                    .cloned()
                    .map(|info| [(*account, info)])
                    .unwrap_or_default();
                let _ = self.rai_epoch_manager.record_drain_evidence(
                    closing.epoch,
                    &root,
                    evidence,
                    segment,
                );
                continue;
            }
            let Some(entry) = self.roots.get(&root) else {
                continue;
            };
            let evidence = entry.election.rai_votes.clone();
            let released = self
                .rai_epoch_manager
                .happy_path_drain(closing.epoch)
                .and_then(|drain| {
                    let mut probe = drain.clone();
                    probe.record_persistent_evidence(&root, &evidence)
                })
                .is_some_and(|outcome| {
                    matches!(
                        outcome,
                        crate::consensus::rai::RaiDrainOutcome::ReleasedTimeout
                            | crate::consensus::rai::RaiDrainOutcome::ReleasedConflict
                    )
                });
            if released {
                let _ = self.rai_epoch_manager.record_drain_evidence(
                    closing.epoch,
                    &root,
                    &evidence,
                    [],
                );
            }
        }
        let Some((root, candidate)) = self.rai_epoch_manager.begin_close_record() else {
            return;
        };
        let Some(committee) = self.rai_epoch_manager.close_committee(closing.epoch) else {
            return;
        };
        let _ = self.insert_close_record(
            super::RaiCloseElectionSpec {
                id: RaiCloseElectionId {
                    kind: RaiCloseKind::Record,
                    epoch: closing.epoch,
                    round: 0,
                },
                root,
                candidate,
                committee,
            },
            now,
        );
    }

    #[cfg(feature = "rai_protocol")]
    fn process_rai_event(
        &mut self,
        event: crate::consensus::rai::RaiEpochEvent,
        local_key: &rsnano_types::PrivateKey,
        epoch_duration: Duration,
        now: Timestamp,
    ) -> Vec<crate::consensus::rai::RaiReport> {
        use crate::consensus::rai::{RaiEpochLoop, RaiEpochLoopDriver};

        #[derive(Default)]
        struct LiveDriver {
            reports: Vec<crate::consensus::rai::RaiReport>,
            visible: std::collections::BTreeMap<
                rsnano_types::RaiEpoch,
                std::collections::BTreeSet<QualifiedRoot>,
            >,
            close_evidence: Option<crate::consensus::rai::RaiElectionVoteState>,
            close_winner: Option<BlockHash>,
            close_elections: Vec<(
                crate::consensus::rai::RaiCloseKind,
                rsnano_types::RaiEpoch,
                u32,
                QualifiedRoot,
                BlockHash,
            )>,
        }

        impl RaiEpochLoopDriver for LiveDriver {
            fn visible_obligations(
                &self,
                epoch: rsnano_types::RaiEpoch,
            ) -> std::collections::BTreeSet<QualifiedRoot> {
                self.visible.get(&epoch).cloned().unwrap_or_default()
            }

            fn vote_visible_obligations(
                &self,
                _epoch: rsnano_types::RaiEpoch,
            ) -> std::collections::BTreeSet<QualifiedRoot> {
                // Local election presence is not the authenticated >F
                // vote-visibility witness required by the protocol.
                Default::default()
            }

            fn start_close_election(
                &mut self,
                kind: crate::consensus::rai::RaiCloseKind,
                epoch: rsnano_types::RaiEpoch,
                round: u32,
                root: QualifiedRoot,
                hash: BlockHash,
            ) {
                self.close_elections.push((kind, epoch, round, root, hash));
            }

            fn close_election_winner(
                &self,
                _kind: crate::consensus::rai::RaiCloseKind,
                _epoch: rsnano_types::RaiEpoch,
                _round: u32,
            ) -> Option<BlockHash> {
                self.close_winner
            }

            fn close_election_evidence(
                &self,
                _kind: crate::consensus::rai::RaiCloseKind,
                _epoch: rsnano_types::RaiEpoch,
                _round: u32,
            ) -> Option<crate::consensus::rai::RaiElectionVoteState> {
                self.close_evidence.clone()
            }

            fn broadcast_report(&mut self, report: crate::consensus::rai::RaiReport) {
                self.reports.push(report);
            }
        }

        // Snapshot the changed election before the loop may ask the manager to
        // derive a decision, death proof, or live carry from it.
        let (close_evidence, close_winner) = match &event {
            crate::consensus::rai::RaiEpochEvent::CloseElectionChanged { kind, epoch, round } => {
                let root = match kind {
                    crate::consensus::rai::RaiCloseKind::Cut => {
                        crate::consensus::rai::rai_close_cut_root(*epoch, *round)
                    }
                    crate::consensus::rai::RaiCloseKind::Record => {
                        crate::consensus::rai::rai_close_record_root(*epoch, *round)
                    }
                };
                self.roots.get(&root).map_or((None, None), |entry| {
                    let evidence = entry.election.rai_votes.clone();
                    let winner = match evidence.outcome {
                        crate::consensus::rai::RaiOutcome::Confirmed(hash) => Some(hash),
                        _ => None,
                    };
                    (Some(evidence), winner)
                })
            }
            _ => (None, None),
        };
        let mut visible = self.rai_visible_obligations.clone();
        for entry in self.roots.iter() {
            if entry.election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot {
                visible
                    .entry(entry.election.rai_epoch())
                    .or_default()
                    .insert(entry.root.clone());
            }
        }

        // Keep one source of truth: this is the same manager used by active
        // elections and the rai_status RPC, moved through the loop for a tick.
        let replacement = crate::consensus::rai::RaiEpochManager::new(
            std::sync::Arc::new(RepWeights::default()),
            BlockHash::ZERO,
        );
        let manager = std::mem::replace(&mut self.rai_epoch_manager, replacement);
        let started_at = manager.state().open_started_at;
        let mut epoch_loop = RaiEpochLoop::new(
            manager,
            LiveDriver {
                close_evidence,
                close_winner,
                visible,
                ..Default::default()
            },
            local_key.clone(),
            epoch_duration,
            started_at,
        );
        epoch_loop.process(event);
        let (manager, driver) = epoch_loop.into_parts();
        self.rai_epoch_manager = manager;
        for (kind, epoch, round, root, candidate) in driver.close_elections {
            let Some(committee) = self.rai_epoch_manager.close_committee(epoch) else {
                continue;
            };
            let spec = super::RaiCloseElectionSpec {
                id: crate::consensus::rai::RaiCloseElectionId { kind, epoch, round },
                root,
                candidate,
                committee,
            };
            let _ = self.insert_close_election(spec, now);
        }
        driver.reports
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_epoch_state(&self) -> &crate::consensus::rai::RaiEpochState {
        self.rai_epoch_manager.state()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_installed_close_hash(&self, epoch: rsnano_types::RaiEpoch) -> Option<BlockHash> {
        self.rai_epoch_manager.installed_close_hash(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_decided_cut_hashes(
        &self,
    ) -> &std::collections::BTreeMap<rsnano_types::RaiEpoch, BlockHash> {
        self.rai_epoch_manager.decided_cut_hashes()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_happy_path_drains(
        &self,
    ) -> &std::collections::BTreeMap<rsnano_types::RaiEpoch, crate::consensus::rai::RaiHappyPathDrain>
    {
        self.rai_epoch_manager.happy_path_drains()
    }
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            roots: RootContainer::new(config.max_elections),
            observer: None,
            stopped: false,
            count_by_behavior: Default::default(),
            base_latency,
            recently_confirmed: RecentlyConfirmedCache::new(config.confirmation_cache),
            cooldown: CooldownController::default(),
            max_elections: config.max_elections,
            max_elections_per_bucket: max(config.max_elections / bucket_count(), 1),
            stats: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_epoch_manager: crate::consensus::rai::RaiEpochManager::new(
                std::sync::Arc::new(RepWeights::default()),
                BlockHash::ZERO,
            ),
            #[cfg(feature = "rai_protocol")]
            rai_visible_obligations: Default::default(),
            #[cfg(feature = "rai_protocol")]
            rai_terminal_slots: Default::default(),
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_with_rai_committee(
        config: ActiveElectionsConfig,
        base_latency: Duration,
        genesis_committee: std::sync::Arc<RepWeights>,
        genesis_governing_hash: BlockHash,
    ) -> Self {
        let mut result = Self::new(config, base_latency);
        result.rai_epoch_manager =
            crate::consensus::rai::RaiEpochManager::new(genesis_committee, genesis_governing_hash);
        result
    }

    pub fn set_observer(&mut self, observer: Sender<AecFact>) {
        self.observer = Some(observer);
    }

    pub fn max_len(&self) -> usize {
        self.max_elections
    }

    pub fn count_by_behavior(&self, behavior: ElectionBehavior) -> usize {
        self.count_by_behavior[behavior as usize]
    }

    fn count_by_behavior_mut(&mut self, behavior: ElectionBehavior) -> &mut usize {
        &mut self.count_by_behavior[behavior as usize]
    }

    pub fn bucket_len(&self, bucket_id: usize) -> usize {
        self.roots.bucket_len(bucket_id)
    }

    pub fn find_bucket(&self, root: &QualifiedRoot) -> Option<usize> {
        self.roots.find_bucket(root)
    }

    pub fn lowest_priority(&self, bucket_id: usize) -> Option<(QualifiedRoot, TimePriority)> {
        self.roots.lowest_priority(bucket_id)
    }

    /// Iterates over all elections in round robin fashion starting at the highest bucket
    pub fn iter_round_robin(&self) -> impl Iterator<Item = &Election> {
        self.roots.round_robin().map(|i| &i.election)
    }

    pub fn check_vacancy<T>(&self, source: &T) -> bool
    where
        T: ElectionCandidateSource,
    {
        let bucket_infos = self.roots.bucket_infos();
        source.should_schedule(&bucket_infos)
    }

    pub fn insert(
        &mut self,
        request: AecInsertRequest,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.ensure_not_stopped()?;
        self.ensure_not_recently_confirmed(&request)?;

        if self.try_upgrade_priority_election(&request)? {
            return Ok(());
        }

        self.insert_new_election(request, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_election(
        &mut self,
        spec: super::RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        use crate::consensus::rai::{RaiCloseKind, rai_close_cut_root, rai_close_record_root};

        self.ensure_not_stopped()?;
        let tracker = match spec.id.kind {
            RaiCloseKind::Cut => self.rai_epoch_manager.close_cut_tracker(spec.id.epoch),
            RaiCloseKind::Record => self.rai_epoch_manager.close_record_tracker(spec.id.epoch),
        };
        let expected_root = match spec.id.kind {
            RaiCloseKind::Cut => rai_close_cut_root(spec.id.epoch, 0),
            RaiCloseKind::Record => rai_close_record_root(spec.id.epoch, 0),
        };
        if spec.id.round != 0
            || spec.root != expected_root
            || tracker
                .and_then(|tracker| tracker.round(0))
                .is_none_or(|round| {
                    round.id != spec.id
                        || round.candidates.len() != 1
                        || !round.validated_preimages.contains(&spec.candidate)
                })
            || self
                .rai_epoch_manager
                .close_committee(spec.id.epoch)
                .is_none_or(|committee| committee != spec.committee)
        {
            return Err(AecInsertError::InvalidRaiCloseElection);
        }
        if self.roots.get(&spec.root).is_some() || self.roots.vote_router.is_active(&spec.candidate)
        {
            return Err(AecInsertError::Duplicate);
        }

        let root = spec.root;
        let candidate = spec.candidate;
        let election = Election::new_close(
            spec.id,
            root.clone(),
            candidate,
            spec.committee,
            self.base_latency,
            now,
        );
        self.roots.insert(Entry {
            root: root.clone(),
            election,
            priority: rsnano_types::BlockPriority::default(),
        });
        *self.count_by_behavior_mut(ElectionBehavior::Manual) += 1;
        self.stats.started(ElectionBehavior::Manual);
        self.notify(AecFact::ElectionStarted(candidate, root));
        Ok(())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_cut(
        &mut self,
        spec: super::RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        if spec.id.kind != crate::consensus::rai::RaiCloseKind::Cut {
            return Err(AecInsertError::InvalidRaiCloseElection);
        }
        self.insert_close_election(spec, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_close_record(
        &mut self,
        spec: super::RaiCloseElectionSpec,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        if spec.id.kind != crate::consensus::rai::RaiCloseKind::Record {
            return Err(AecInsertError::InvalidRaiCloseElection);
        }
        self.insert_close_election(spec, now)
    }

    /// Re-opens a certified-cut obligation which this replica did not have
    /// active when the cut was installed. This is deliberately tied to the
    /// closing epoch (rather than the successor epoch) so replayed durable
    /// votes pass their epoch/governing-hash checks and can complete drain.
    #[cfg(feature = "rai_protocol")]
    pub fn insert_drain_election(
        &mut self,
        block: SavedBlock,
        epoch: rsnano_types::RaiEpoch,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        self.ensure_not_stopped()?;
        let root = block.qualified_root();
        if self.roots.get(&root).is_some()
            || self.rai_terminal_slots.contains_key(&(epoch, root.clone()))
        {
            return Err(AecInsertError::Duplicate);
        }
        if self
            .rai_epoch_manager
            .obligations_to_drain(epoch)
            .is_none_or(|obligations| !obligations.contains(&root))
        {
            return Err(AecInsertError::InvalidRaiCloseElection);
        }
        let governing_hash = self
            .rai_epoch_manager
            .governing_hash(epoch)
            .ok_or(AecInsertError::MissingRaiGoverningClose)?;
        let committees = self
            .rai_epoch_manager
            .slot_committees(epoch)
            .ok_or(AecInsertError::MissingRaiGoverningClose)?;
        let hash = block.hash();
        let election = Election::new_slot(
            block,
            ElectionBehavior::Manual,
            self.base_latency,
            now,
            epoch,
        )
        .with_rai_committees(committees)
        .with_rai_governing_hash(Some(governing_hash));
        self.roots.insert(Entry {
            root: root.clone(),
            election,
            priority: rsnano_types::BlockPriority::default(),
        });
        *self.count_by_behavior_mut(ElectionBehavior::Manual) += 1;
        self.stats.started(ElectionBehavior::Manual);
        self.notify(AecFact::ElectionStarted(hash, root));
        Ok(())
    }

    fn ensure_not_stopped(&self) -> Result<(), AecInsertError> {
        if self.stopped {
            Err(AecInsertError::Stopped)
        } else {
            Ok(())
        }
    }

    fn ensure_not_recently_confirmed(
        &self,
        request: &AecInsertRequest,
    ) -> Result<(), AecInsertError> {
        let root = request.block.qualified_root();

        if self.recently_confirmed.root_exists(&root) {
            return Err(AecInsertError::RecentlyConfirmed);
        }
        Ok(())
    }

    fn try_upgrade_priority_election(
        &mut self,
        request: &AecInsertRequest,
    ) -> Result<bool, AecInsertError> {
        let (upgraded, previous_behavior) = self.roots.try_upgrade_to_priority_election(request);

        if upgraded {
            *self.count_by_behavior_mut(previous_behavior.unwrap()) -= 1;
            *self.count_by_behavior_mut(request.behavior) += 1;
            Ok(true)
        } else if previous_behavior.is_some() {
            Err(AecInsertError::Duplicate)
        } else {
            Ok(false)
        }
    }

    fn insert_new_election(
        &mut self,
        request: AecInsertRequest,
        now: Timestamp,
    ) -> Result<(), AecInsertError> {
        let root = request.block.qualified_root();
        let hash = request.block.hash();
        #[cfg(not(feature = "rai_protocol"))]
        let election = Election::new(request.block, request.behavior, self.base_latency, now);
        #[cfg(feature = "rai_protocol")]
        let epoch_state = self.rai_epoch_manager.state();
        #[cfg(feature = "rai_protocol")]
        let epoch = epoch_state.open_epoch;
        #[cfg(feature = "rai_protocol")]
        let governing_hash = self
            .rai_epoch_manager
            .governing_hash(epoch)
            .ok_or(AecInsertError::MissingRaiGoverningClose)?;
        #[cfg(feature = "rai_protocol")]
        let committees = self
            .rai_epoch_manager
            .slot_committees(epoch)
            .ok_or(AecInsertError::MissingRaiGoverningClose)?;
        #[cfg(feature = "rai_protocol")]
        let election = Election::new_slot(
            request.block,
            request.behavior,
            self.base_latency,
            now,
            epoch,
        )
        .with_rai_committees(committees)
        .with_rai_governing_hash(Some(governing_hash));

        #[cfg(feature = "rai_protocol")]
        self.rai_visible_obligations
            .entry(epoch)
            .or_default()
            .insert(root.clone());

        self.roots.insert(Entry {
            root: root.clone(),
            election,
            priority: request.priority,
        });

        *self.count_by_behavior_mut(request.behavior) += 1;
        self.stats.started(request.behavior);
        self.notify(AecFact::ElectionStarted(hash, root));
        Ok(())
    }

    pub fn try_add_fork(&mut self, fork: &Block, fork_tally: Amount) -> bool {
        let Some(entry) = self.roots.get_mut(&fork.qualified_root()) else {
            return false;
        };

        let result = entry.election.try_add_fork(fork, fork_tally);
        let added = match result {
            AddForkResult::Added => {
                self.notify(AecFact::BlockAddedToElection(fork.hash()));
                true
            }
            AddForkResult::Replaced(removed) => {
                self.roots.vote_router.disconnect(&removed.hash());
                self.notify(AecFact::BlockDiscarded(removed.into()));
                self.notify(AecFact::BlockAddedToElection(fork.hash()));
                true
            }
            AddForkResult::TallyTooLow => {
                self.notify(AecFact::BlockDiscarded(fork.clone()));
                false
            }
            AddForkResult::Duplicate | AddForkResult::ElectionEnded => false,
        };

        if added {
            self.roots
                .vote_router
                .connect(fork.hash(), fork.qualified_root());
            self.stats.conflicts += 1;
        }

        added
    }

    /// How many election slots are available
    /// This is a soft limit and can be negative!
    pub fn vacancy(&self) -> i64 {
        if self.cooldown.is_cooling_down() {
            return 0;
        }
        let current_size = self.roots.len() as i64;
        self.max_elections as i64 - current_size
    }

    pub fn set_cooldown(&mut self, cool_down: bool, reason: AecCooldownReason) {
        let result = self.cooldown.set_cooldown(cool_down, reason);
        if result == CooldownResult::Recovered {
            self.notify(AecFact::Recovered);
        }
    }

    pub fn stop(&mut self) {
        // destroy send queue so that the receiver thread will be stopped too
        drop(self.observer.take());
        self.stopped = true;
        self.roots.clear();
    }

    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.roots.get(root).is_some()
    }

    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.roots.vote_router.is_active(block_hash)
    }

    pub fn was_recently_confirmed(&self, block_hash: &BlockHash) -> bool {
        self.recently_confirmed.hash_exists(block_hash)
    }

    pub fn clear_recently_confirmed(&mut self) {
        self.recently_confirmed.clear();
    }

    /// Returns the current active elections after transitioning
    pub fn transition_time(&mut self, now: Timestamp) {
        self.stats.ticked += 1;
        for entry in self.roots.iter_mut() {
            entry.election.transition_time(now);
        }
        self.erase_ended_elections();
    }

    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<&Election> {
        self.roots.election_for_root(root)
    }

    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<&Election> {
        self.roots.election_for_block(block_hash)
    }

    pub fn transition_active(&mut self, block_hash: &BlockHash) -> bool {
        let Some(election) = self.roots.election_for_block_mut(block_hash) else {
            return false;
        };
        election.transition_active();
        true
    }

    pub fn refill<T>(&mut self, source: &mut T, now: Timestamp)
    where
        T: ElectionCandidateSource,
    {
        if self.cooldown.is_cooling_down() {
            return;
        }

        let mut any_inserted = true;
        while any_inserted {
            any_inserted = false;
            for bucket_index in (0..self.roots.bucket_count()).rev() {
                let bucket = &self.roots.bucket_infos()[bucket_index];
                let bucket_vacancy = if self.len() >= self.max_elections {
                    0
                } else {
                    self.max_elections_per_bucket as isize - bucket.election_count as isize
                };

                let Some(candidate) = source.next_candidate(
                    bucket_index,
                    bucket_vacancy,
                    bucket.lowest_priority.time,
                ) else {
                    continue;
                };

                any_inserted = true;
                let root = candidate.block.qualified_root();
                if self.find_bucket(&root) == Some(candidate.bucket_id) {
                    self.stats.activate_failed_duplicate += 1;
                    continue;
                }

                if self.bucket_len(candidate.bucket_id) >= self.max_elections_per_bucket {
                    self.erase_lowest_prio_election(candidate.bucket_id);
                    self.stats.replaced += 1;
                }

                // TODO: Don't hard code priority election!
                match self.insert(
                    AecInsertRequest::new_priority(candidate.block, candidate.priority),
                    now,
                ) {
                    Ok(_) => {
                        self.stats.activate_success += 1;
                    }
                    Err(AecInsertError::RecentlyConfirmed) => {
                        self.stats.activate_failed_confirmed += 1;
                    }
                    Err(AecInsertError::Duplicate) => {
                        self.stats.activate_failed_duplicate += 1;
                    }
                    Err(AecInsertError::MissingRaiGoverningClose) => {}
                    #[cfg(feature = "rai_protocol")]
                    Err(AecInsertError::InvalidRaiCloseElection) => {}
                    Err(AecInsertError::Stopped) => {}
                }
            }
        }
    }

    pub fn remove_votes<'a>(
        &mut self,
        root: &QualifiedRoot,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        let Some(election) = self.roots.election_for_root_mut(root) else {
            return;
        };
        for voter in voters {
            election.remove_vote(voter);
        }
    }

    pub fn erase_ended_elections(&mut self) {
        let removed = self.roots.drain_filter(|i| i.election.state().has_ended());

        for entry in removed {
            self.cleanup_election(entry);
        }
    }

    pub fn erase(&mut self, root: &QualifiedRoot) -> bool {
        let Some(entry) = self.roots.erase(root) else {
            return false;
        };
        self.cleanup_election(entry);
        true
    }

    pub fn erase_lowest_prio_election(&mut self, bucket_id: usize) {
        let Some((root, _)) = self.lowest_priority(bucket_id) else {
            return;
        };
        self.erase(&root);
    }

    fn cleanup_election(&mut self, entry: Entry) {
        let election = &entry.election;

        #[cfg(feature = "rai_protocol")]
        if election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot {
            let confirmed = match election.winner() {
                rsnano_types::MaybeSavedBlock::Saved(block) => Some(
                    rsnano_types::ConfirmationHeightInfo::new(block.height(), block.hash()),
                ),
                rsnano_types::MaybeSavedBlock::Unsaved(_) => None,
            };
            // Every removal path funnels through cleanup. Preserve the signed
            // state here, before vote-router disconnection makes it impossible
            // for close drain to recover an election that ended outside the
            // direct apply_vote confirmation path.
            self.rai_terminal_slots.insert(
                (election.rai_epoch(), entry.root.clone()),
                (election.rai_votes.clone(), election.account(), confirmed),
            );
        }

        // Keep track of election count by election type
        *self.count_by_behavior_mut(election.behavior()) -= 1;

        self.stats.stopped(&entry.election);
        self.notify(AecFact::ElectionEnded(entry.election));
    }

    /// Dependent elections are implicitly confirmed when their block is confirmed
    pub fn confirm_dependent_elections(
        &mut self,
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>)>,
        now: Timestamp,
    ) {
        for (confirmed_block, source_election) in confirmed {
            let confirmed_election =
                self.confirm_dependent_election(&confirmed_block, source_election, now);

            self.block_confirmed(confirmed_block, confirmed_election);
        }
    }

    fn confirm_dependent_election(
        &mut self,
        confirmed_block: &SavedBlock,
        source_election: Option<ConfirmedElection>,
        now: Timestamp,
    ) -> ConfirmedElection {
        // Check if the currently confirmed block was part of an election that triggered
        // the block confirmation
        if let Some(source) = source_election
            && confirmed_block.hash() == source.winner.hash()
        {
            // This is the block that was directly confirmed by the source election.
            // The election is already confirmed, so there is nothing to do.
            return source;
        }

        let Some(corresponding) = self.roots.get_mut(&confirmed_block.qualified_root()) else {
            return ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::InactiveConfirmationHeight,
            );
        };

        if corresponding.election.winner().hash() == confirmed_block.hash() {
            corresponding.election.force_confirm();
            corresponding
                .election
                .into_confirmed_election(now, ConfirmationType::ActiveConfirmationHeight)
        } else {
            corresponding.election.cancel();
            ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::ActiveConfirmationHeight,
            )
        }
    }

    fn block_confirmed(&mut self, block: SavedBlock, election: ConfirmedElection) {
        self.stats.block_confirmations[election.confirmation_type as usize] += 1;
        self.notify(AecFact::BlockConfirmed(block, election));
    }

    pub fn remove_recently_confirmed(&mut self, block_hash: &BlockHash) {
        self.recently_confirmed.erase(block_hash);
    }

    pub fn apply_vote<'a>(
        &mut self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        let mut apply_helper = ApplyVoteHelper {
            args: &args,
            recently_confirmed: &mut self.recently_confirmed,
            vote_counter: &mut self.stats.vote_counter,
            observer: &self.observer,
            roots: &mut self.roots,
        };
        let result = apply_helper.apply_vote();
        for entry in result.confirmed {
            #[cfg(feature = "rai_protocol")]
            if entry.election.rai_kind() == crate::consensus::election::RaiElectionKind::Slot {
                let confirmed = match entry.election.winner() {
                    rsnano_types::MaybeSavedBlock::Saved(block) => Some(
                        rsnano_types::ConfirmationHeightInfo::new(block.height(), block.hash()),
                    ),
                    rsnano_types::MaybeSavedBlock::Unsaved(_) => None,
                };
                self.rai_terminal_slots.insert(
                    (entry.election.rai_epoch(), entry.root.clone()),
                    (
                        entry.election.rai_votes.clone(),
                        entry.election.account(),
                        confirmed,
                    ),
                );
            }
            #[cfg(feature = "rai_protocol")]
            if matches!(
                entry.election.rai_kind(),
                crate::consensus::election::RaiElectionKind::CloseCut
                    | crate::consensus::election::RaiElectionKind::CloseRecord
            ) {
                let epoch = entry.election.rai_epoch();
                let round = entry.election.rai_round();
                let candidate = entry.election.rai_votes.outcome;
                let evidence = entry.election.rai_votes.clone();
                let stored = match entry.election.rai_kind() {
                    crate::consensus::election::RaiElectionKind::CloseCut => self
                        .rai_epoch_manager
                        .store_close_cut_evidence(epoch, round, evidence),
                    crate::consensus::election::RaiElectionKind::CloseRecord => self
                        .rai_epoch_manager
                        .store_close_record_evidence(epoch, round, evidence),
                    crate::consensus::election::RaiElectionKind::Slot => false,
                };
                if stored {
                    if let crate::consensus::rai::RaiOutcome::Confirmed(hash) = candidate {
                        match entry.election.rai_kind() {
                            crate::consensus::election::RaiElectionKind::CloseCut => {
                                let _ = self.rai_epoch_manager.decide_close_cut(epoch, round, hash);
                            }
                            crate::consensus::election::RaiElectionKind::CloseRecord => {
                                if let Ok(frontiers) =
                                    self.rai_epoch_manager.install_certified_close_record(
                                        epoch,
                                        round,
                                        hash,
                                        args.rep_weights.clone(),
                                    )
                                {
                                    let frontiers = frontiers.clone();
                                    self.notify(AecFact::RaiCloseInstalled(frontiers));
                                }
                            }
                            crate::consensus::election::RaiElectionKind::Slot => {}
                        }
                    }
                }
            }
            self.cleanup_election(entry);
        }
        result.per_block
    }

    pub fn force_confirm(&mut self, block_hash: &BlockHash, now: Timestamp) {
        let Some(election) = self.roots.election_for_block_mut(block_hash) else {
            panic!("Force confirm failed, because no active election was found");
        };
        if election.force_confirm() {
            let confirmed_election =
                election.into_confirmed_election(now, ConfirmationType::ActiveConfirmedQuorum);
            self.notify(AecFact::ElectionConfirmed(confirmed_election));
        }
    }

    pub fn cancel(&mut self, root: &QualifiedRoot) {
        if let Some(entry) = self.roots.get_mut(root) {
            entry.election.cancel();
        }
    }

    pub fn cancel_all(&mut self) {
        for entry in self.roots.iter_mut() {
            entry.election.cancel();
        }
    }

    pub fn len(&self) -> usize {
        self.roots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn info(&self, now: Timestamp) -> ActiveElectionsInfo {
        ActiveElectionsInfo {
            max_elections: self.max_elections,
            total: self.roots.len(),
            stale: self
                .roots
                .iter()
                .filter(|i| i.election.start().elapsed(now) >= Duration::from_secs(60))
                .count(),
            priority: self.count_by_behavior(ElectionBehavior::Priority),
            hinted: self.count_by_behavior(ElectionBehavior::Hinted),
            optimistic: self.count_by_behavior(ElectionBehavior::Optimistic),
        }
    }

    pub fn simulate_event(&self, event: AecFact) {
        self.notify(event);
    }

    pub fn snapshot(&self, now: Timestamp) -> AecSnapshot {
        self.roots.snapshot(now)
    }

    fn notify(&self, event: AecFact) {
        if let Some(sender) = &self.observer {
            sender.send(event).unwrap()
        }
    }
}

impl Default for ActiveElectionsContainer {
    fn default() -> Self {
        Self::new(ActiveElectionsConfig::default(), Duration::from_secs(1))
    }
}

impl StatsSource for ActiveElectionsContainer {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.cooldown.collect_stats(result);
        self.stats.collect_stats(result);
    }
}

impl ContainerInfoProvider for ActiveElectionsContainer {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .leaf("roots", self.roots.len(), RootContainer::ELEMENT_SIZE)
            .leaf(
                "normal",
                self.count_by_behavior(ElectionBehavior::Priority),
                0,
            )
            .leaf(
                "hinted".to_string(),
                self.count_by_behavior(ElectionBehavior::Hinted),
                0,
            )
            .leaf(
                "optimistic".to_string(),
                self.count_by_behavior(ElectionBehavior::Optimistic),
                0,
            )
            .node(
                "recently_confirmed",
                self.recently_confirmed.container_info(),
            )
            .node("vote_router", self.roots.vote_router.container_info())
            .node("buckets", self.roots.container_info())
            .finish()
    }
}

pub struct ApplyVoteArgs<'a> {
    pub vote: &'a FilteredVote,
    pub rep_weights: &'a RepWeights,
    pub quorum_snapshot: &'a QuorumSnapshot,
    pub now: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "rai_protocol"))]
    use crate::consensus::ReceivedVote;
    use rsnano_types::{BlockPriority, TimePriority};
    #[cfg(not(feature = "rai_protocol"))]
    use rsnano_types::{PrivateKey, Vote, VoteDelivery};
    #[cfg(not(feature = "rai_protocol"))]
    use std::sync::Arc;

    #[test]
    fn empty() {
        let container = ActiveElectionsContainer::default();
        assert_eq!(container.len(), 0);
        assert!(!container.is_active_root(&QualifiedRoot::new_test_instance()));
        assert!(!container.is_active_hash(&BlockHash::from(1)));
    }

    #[test]
    fn insert_election() {
        let mut container = ActiveElectionsContainer::default();
        let request = AecInsertRequest {
            block: SavedBlock::new_test_instance(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        container
            .insert(request, Timestamp::new_test_instance())
            .unwrap();

        assert_eq!(container.len(), 1);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn inserted_block_election_is_a_slot_with_epoch_fixed_at_creation() {
        use crate::consensus::{election::RaiElectionKind, rai::RaiEpoch};

        let mut container = ActiveElectionsContainer::default();
        assert!(
            container
                .rai_epoch_manager
                .start_closing(Timestamp::new_test_instance())
        );
        assert_eq!(
            container.rai_epoch_manager.closing_epoch().unwrap().epoch,
            RaiEpoch::ZERO
        );
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();

        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                Timestamp::new_test_instance(),
            )
            .unwrap();

        container.rai_epoch_manager.open_epoch(RaiEpoch::new(2));
        let election = container.election_for_root(&root).unwrap();
        assert_eq!(election.qualified_root(), &root);
        assert_eq!(election.rai_kind(), RaiElectionKind::Slot);
        assert_eq!(election.rai_epoch(), RaiEpoch::new(1));
        assert_eq!(election.rai_round(), 0);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn confirmed_slot_is_retained_until_the_close_drain_consumes_it() {
        use crate::consensus::{FilteredVote, ReceivedVote, rai::RaiReport};
        use rsnano_types::{
            Amount, ConfirmationHeightInfo, PrivateKey, RaiCommitteeScope, RaiVoteMetadata,
            RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee,
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let hash = block.hash();
        let account = block.account();
        let expected_frontier = ConfirmationHeightInfo::new(block.height(), hash);
        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                },
                now,
            )
            .unwrap();

        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                vec![hash],
                RaiVoteMetadata {
                    phase: RaiVotePhase::First,
                    epoch: 0.into(),
                    governing_hash: BlockHash::from(7),
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        assert_eq!(
            container.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: &RepWeights::default(),
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            })[&hash],
            Ok(())
        );
        assert!(container.election_for_root(&root).is_none());
        assert!(
            container
                .rai_terminal_slots
                .contains_key(&(0.into(), root.clone()))
        );

        container.rai_epoch_manager.start_closing(now);
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(RaiReport::new(&key, 0.into(), [root.clone()]))
            .unwrap();
        let (_, cut) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(0.into(), 0, cut)
            .unwrap();

        container.rai_progress_close(
            [(account, expected_frontier.clone())].into_iter().collect(),
            now,
        );

        let drain = container
            .rai_epoch_manager
            .happy_path_drain(0.into())
            .unwrap();
        assert!(drain.is_complete());
        assert_eq!(drain.finalized.get(&root), Some(&hash));
        assert_eq!(
            container
                .rai_epoch_manager
                .drain_frontiers(0.into())
                .unwrap()[&account],
            expected_frontier
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epoch_two_election_requires_close_zero() {
        use crate::consensus::rai::RaiEpoch;

        let mut container = ActiveElectionsContainer::default();
        container.rai_epoch_manager.open_epoch(RaiEpoch::new(2));

        let result = container.insert(
            AecInsertRequest {
                block: SavedBlock::new_test_instance(),
                behavior: ElectionBehavior::Priority,
                priority: BlockPriority::new_test_instance(),
            },
            Timestamp::new_test_instance(),
        );

        assert_eq!(result, Err(AecInsertError::MissingRaiGoverningClose));
        assert_eq!(container.len(), 0);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_cut_uses_normal_vote_validation_and_enters_draining() {
        use crate::consensus::{
            FilteredVote, RaiCloseElectionSpec, ReceivedVote,
            election::RaiElectionKind,
            rai::{
                RaiCloseElectionId, RaiCloseKind, RaiClosingPhase, RaiReport, rai_close_cut_root,
            },
        };
        use rsnano_types::{
            Amount, PrivateKey, RaiCommitteeScope, RaiVoteMetadata, RaiVotePhase,
            UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let keys = [
            PrivateKey::from(1),
            PrivateKey::from(2),
            PrivateKey::from(3),
            PrivateKey::from(4),
        ];
        let committee = std::sync::Arc::new(RepWeights::from([
            (keys[0].public_key(), Amount::raw(1)),
            (keys[1].public_key(), Amount::raw(1)),
            (keys[2].public_key(), Amount::raw(1)),
            (keys[3].public_key(), Amount::raw(1)),
        ]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        assert!(container.rai_epoch_manager.start_closing(now));
        for key in &keys {
            container
                .rai_epoch_manager
                .reports_mut()
                .insert(RaiReport::new(key, 0.into(), []))
                .unwrap();
        }
        let (root, candidate) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        let id = RaiCloseElectionId {
            kind: RaiCloseKind::Cut,
            epoch: 0.into(),
            round: 0,
        };
        container
            .insert_close_cut(
                RaiCloseElectionSpec {
                    id,
                    root: root.clone(),
                    candidate,
                    committee,
                },
                now,
            )
            .unwrap();

        let election = container.election_for_root(&root).unwrap();
        assert_eq!(election.rai_kind(), RaiElectionKind::CloseCut);
        assert!(election.candidate_blocks().is_empty());
        assert_eq!(root, rai_close_cut_root(0.into(), 0));

        let rep_weights = RepWeights::default();
        let quorum = QuorumSnapshot::new_test_instance();
        for key in &keys {
            let vote: FilteredVote = ReceivedVote::new(
                Vote::new_rai(
                    key,
                    UnixMillisTimestamp::new(1),
                    0,
                    vec![candidate],
                    RaiVoteMetadata {
                        phase: RaiVotePhase::First,
                        epoch: 0.into(),
                        governing_hash: BlockHash::from(999),
                        scope: RaiCommitteeScope::All,
                    },
                )
                .into(),
                VoteDelivery::Direct,
                None,
            )
            .into();
            assert_eq!(
                container.apply_vote(ApplyVoteArgs {
                    vote: &vote,
                    rep_weights: &rep_weights,
                    quorum_snapshot: &quorum,
                    now,
                })[&candidate],
                Ok(())
            );
        }

        assert_eq!(
            container.rai_epoch_manager.decided_close_hash(0.into()),
            Some(candidate)
        );
        assert_eq!(
            container.rai_epoch_manager.closing_epoch().unwrap().phase,
            RaiClosingPhase::Draining
        );
        assert_eq!(
            container
                .rai_epoch_manager
                .close_cut_tracker(0.into())
                .unwrap()
                .round(0)
                .unwrap()
                .id,
            id
        );
        assert_eq!(
            container
                .rai_epoch_manager
                .decide_close_cut(0.into(), 0, BlockHash::from(123)),
            Err(crate::consensus::rai::CloseCutDecisionError::ImmutableDecision)
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_record_uses_normal_votes_and_closes_epoch_zero() {
        use crate::consensus::{
            FilteredVote, RaiCloseElectionSpec, ReceivedVote,
            election::RaiElectionKind,
            rai::{RaiCloseElectionId, RaiCloseKind, RaiReport, rai_close_record_root},
        };
        use rsnano_types::{
            Account, Amount, ConfirmationHeightInfo, PrivateKey, RaiCommitteeScope,
            RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp, Vote, VoteDelivery,
        };

        let key = PrivateKey::from(1);
        let committee =
            std::sync::Arc::new(RepWeights::from([(key.public_key(), Amount::raw(100))]));
        let mut container = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_secs(1),
            committee.clone(),
            BlockHash::from(7),
        );
        let now = Timestamp::new_test_instance();
        container.rai_epoch_manager.start_closing(now);
        container
            .rai_epoch_manager
            .reports_mut()
            .insert(RaiReport::new(&key, 0.into(), []))
            .unwrap();
        let (_, cut) = container.rai_epoch_manager.begin_cut_election([]).unwrap();
        container
            .rai_epoch_manager
            .install_cut(0.into(), 0, cut)
            .unwrap();
        container.rai_epoch_manager.initialize_drain_frontiers(
            0.into(),
            [(
                Account::from(1),
                ConfirmationHeightInfo::new(4, BlockHash::from(40)),
            )],
        );
        let (root, candidate) = container.rai_epoch_manager.begin_close_record().unwrap();
        let id = RaiCloseElectionId {
            kind: RaiCloseKind::Record,
            epoch: 0.into(),
            round: 0,
        };
        container
            .insert_close_record(
                RaiCloseElectionSpec {
                    id,
                    root: root.clone(),
                    candidate,
                    committee,
                },
                now,
            )
            .unwrap();

        assert_eq!(root, rai_close_record_root(0.into(), 0));
        assert_eq!(
            container.election_for_root(&root).unwrap().rai_kind(),
            RaiElectionKind::CloseRecord
        );
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &key,
                UnixMillisTimestamp::new(1),
                0,
                vec![candidate],
                RaiVoteMetadata {
                    phase: RaiVotePhase::First,
                    epoch: 0.into(),
                    governing_hash: BlockHash::from(999),
                    scope: RaiCommitteeScope::All,
                },
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        assert_eq!(
            container.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: &RepWeights::default(),
                quorum_snapshot: &QuorumSnapshot::new_test_instance(),
                now,
            })[&candidate],
            Ok(())
        );
        assert_eq!(
            container.rai_epoch_manager.installed_close_hash(0.into()),
            Some(candidate)
        );
        assert_eq!(
            container.rai_epoch_manager.state().closed_through,
            Some(crate::consensus::rai::RaiEpoch::ZERO)
        );
        assert_eq!(
            container.rai_epoch_manager.state().open_epoch,
            crate::consensus::rai::RaiEpoch::new(1)
        );
        assert_eq!(
            container.rai_epoch_manager.committee_at(0).unwrap(),
            std::sync::Arc::new(RepWeights::default())
        );
        assert!(container.election_for_root(&root).is_none());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn unknown_close_cut_preimage_cannot_be_voted_for() {
        use crate::consensus::{FilteredVote, ReceivedVote};
        use rsnano_types::{PrivateKey, RaiVoteMetadata, UnixMillisTimestamp, Vote, VoteDelivery};

        let mut container = ActiveElectionsContainer::default();
        let unknown = BlockHash::from(999);
        let vote: FilteredVote = ReceivedVote::new(
            Vote::new_rai(
                &PrivateKey::from(1),
                UnixMillisTimestamp::new(1),
                0,
                vec![unknown],
                RaiVoteMetadata::default(),
            )
            .into(),
            VoteDelivery::Direct,
            None,
        )
        .into();
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &RepWeights::default(),
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now: Timestamp::new_test_instance(),
        });

        assert_eq!(result[&unknown], Err(VoteError::Indeterminate));
    }

    #[cfg(not(feature = "rai_protocol"))]
    #[test]
    fn confirm_election() {
        let mut container = ActiveElectionsContainer::default();

        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();

        let request = AecInsertRequest {
            block,
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        let now = Timestamp::new_test_instance();
        container.insert(request, now).unwrap();

        let rep_key = PrivateKey::from(1);
        let received_vote = test_final_vote(&rep_key, block_hash);

        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);

        let result = container.apply_vote(ApplyVoteArgs {
            vote: &received_vote.into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        assert_eq!(result.get(&block_hash), Some(&Ok(())));

        assert!(container.election_for_block(&block_hash).is_none());
    }

    #[test]
    fn iter_round_robin() {
        let block_a = SavedBlock::new_test_instance_with_key(1);
        let block_b = SavedBlock::new_test_instance_with_key(2);
        let block_c = SavedBlock::new_test_instance_with_key(3);
        let block_d = SavedBlock::new_test_instance_with_key(4);

        let prio_a = BlockPriority::new(Amount::nano(1), TimePriority::new(100));
        let prio_b = BlockPriority::new(Amount::nano(100), TimePriority::new(100));
        let prio_c = BlockPriority::new(Amount::nano(100), TimePriority::new(99));
        let prio_d = BlockPriority::new(Amount::nano(1_000_000), TimePriority::new(100));

        test_iter(&[], &[]);

        test_iter(&[(&block_a, prio_a)], &[&block_a]);

        test_iter(
            &[
                (&block_d, prio_d),
                (&block_a, prio_a),
                (&block_c, prio_c),
                (&block_b, prio_b),
            ],
            &[&block_d, &block_c, &block_a, &block_b],
        )
    }

    #[test]
    fn reports_stale_election_count() {
        let mut container = ActiveElectionsContainer::default();
        let request = AecInsertRequest {
            block: SavedBlock::new_test_instance(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        let start = Timestamp::new_test_instance();

        container.insert(request, start).unwrap();

        assert_eq!(container.info(start).stale, 0);
        assert_eq!(container.info(start + Duration::from_secs(60)).stale, 1);
    }

    #[cfg(not(feature = "rai_protocol"))]
    fn test_final_vote(rep_key: &PrivateKey, block_hash: BlockHash) -> ReceivedVote {
        let vote = Arc::new(Vote::new_final(rep_key, vec![block_hash]));
        ReceivedVote::new(vote, VoteDelivery::Direct, None)
    }

    fn test_iter(blocks: &[(&SavedBlock, BlockPriority)], expected: &[&SavedBlock]) {
        let mut container = ActiveElectionsContainer::default();

        for (block, prio) in blocks {
            let request = AecInsertRequest::new_priority((**block).clone(), *prio);

            container
                .insert(request, Timestamp::new_test_instance())
                .unwrap();
        }

        let result: Vec<_> = container
            .iter_round_robin()
            .map(|i| i.winner().hash())
            .collect();
        let expected: Vec<_> = expected.iter().map(|i| i.hash()).collect();
        assert_eq!(result, expected);
    }
}
