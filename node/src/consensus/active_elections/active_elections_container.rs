#[cfg(feature = "rai_protocol")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    cmp::max,
    collections::{HashMap, HashSet},
    time::Duration,
};

use strum::EnumCount;

use rsnano_ledger::RepWeights;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Amount, Block, BlockHash, BlockPriority, MaybeSavedBlock, PublicKey, QualifiedRoot, SavedBlock,
    TimePriority, VoteError,
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
    rai_epoch: RaiEpoch,
    #[cfg(feature = "rai_protocol")]
    closing_cut: Option<(u64, std::collections::HashSet<rsnano_types::SlotRoot>)>,
    #[cfg(feature = "rai_protocol")]
    finalized_by_epoch: HashMap<u64, HashMap<rsnano_types::SlotRoot, BlockHash>>,
    #[cfg(feature = "rai_protocol")]
    received_epochs: HashMap<BlockHash, u64>,
    #[cfg(feature = "rai_protocol")]
    decided_cuts: HashSet<u64>,
    sealed_epochs: HashSet<u64>,
}

#[cfg(feature = "rai_protocol")]
#[derive(Default)]
pub struct RaiEpoch {
    current: AtomicU64,
}

#[cfg(feature = "rai_protocol")]
impl RaiEpoch {
    pub fn new() -> Self {
        Self {
            current: AtomicU64::new(1),
        }
    }
    pub fn current(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }
    pub fn advance(&self) -> u64 {
        self.current.fetch_add(1, Ordering::AcqRel) + 1
    }
}

impl ActiveElectionsContainer {
    #[cfg(feature = "rai_protocol")]
    pub fn earliest_finalization(&self, slot: rsnano_types::SlotRoot) -> Option<(u64, BlockHash)> {
        self.finalized_by_epoch.iter()
            .filter_map(|(epoch, values)| values.get(&slot).map(|hash| (*epoch, *hash)))
            .min_by_key(|(epoch, _)| *epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn is_active_hash_in_epoch(&self, epoch: u64, hash: &BlockHash) -> bool {
        self.roots.election_for_block(hash, epoch).is_some()
    }
    #[cfg(feature = "rai_protocol")]
    fn request_root(&self, request: &AecInsertRequest) -> QualifiedRoot {
        let root = request.block.qualified_root();
        root.clone().with_epoch(
            request.epoch.unwrap_or_else(|| {
                self.received_epochs
                    .get(&request.block.hash())
                    .copied()
                    .filter(|epoch| {
                        !self.decided_cuts.contains(epoch) && !self.sealed_epochs.contains(epoch)
                    })
                    .unwrap_or_else(|| self.epoch_for_slot(root.slot()))
            }),
        )
    }

    #[cfg(feature = "rai_protocol")]
    pub fn observe_block_receipt(&mut self, hash: BlockHash) {
        // Retransmissions must not move a queued block into a newer epoch.
        self.received_epochs
            .entry(hash)
            .or_insert(self.rai_epoch.current());
    }

    #[cfg(feature = "rai_protocol")]
    pub fn mark_epoch_cut_decided(&mut self, epoch: u64) {
        self.decided_cuts.insert(epoch);
        self.received_epochs.retain(|_, received| *received != epoch);
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
            rai_epoch: RaiEpoch::new(),
            #[cfg(feature = "rai_protocol")]
            closing_cut: None,
            #[cfg(feature = "rai_protocol")]
            finalized_by_epoch: Default::default(),
            #[cfg(feature = "rai_protocol")]
            received_epochs: Default::default(),
            #[cfg(feature = "rai_protocol")]
            decided_cuts: Default::default(),
            sealed_epochs: Default::default(),
        }
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
        #[cfg(feature = "rai_protocol")]
        if self.len() >= self.max_elections {
            return false;
        }
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
        #[cfg(feature = "rai_protocol")]
        self.ensure_no_earlier_epoch_election(&request)?;

        let root = request.block.qualified_root();
        #[cfg(feature = "rai_protocol")]
        let root = self.request_root(&request);
        if self.try_upgrade_priority_election(&request, root)? {
            return Ok(());
        }

        self.insert_new_election(request, now);
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
        #[cfg(feature = "rai_protocol")]
        let root = self.request_root(request);

        #[cfg(feature = "rai_protocol")]
        if self
            .finalized_by_epoch
            .iter()
            .any(|(epoch, finalized)| *epoch < root.epoch && finalized.contains_key(&root.slot()))
        {
            return Err(AecInsertError::RecentlyConfirmed);
        }

        if self.recently_confirmed.root_exists(&root) {
            return Err(AecInsertError::RecentlyConfirmed);
        }
        Ok(())
    }

    fn try_upgrade_priority_election(
        &mut self,
        request: &AecInsertRequest,
        root: QualifiedRoot,
    ) -> Result<bool, AecInsertError> {
        let (upgraded, previous_behavior) =
            self.roots.try_upgrade_to_priority_election(request, root);

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

    #[cfg(feature = "rai_protocol")]
    fn ensure_no_earlier_epoch_election(
        &self,
        request: &AecInsertRequest,
    ) -> Result<(), AecInsertError> {
        let root = self.request_root(request);
        if self.roots.has_earlier_epoch(&root) {
            Err(AecInsertError::Duplicate)
        } else {
            Ok(())
        }
    }

    fn insert_new_election(&mut self, request: AecInsertRequest, now: Timestamp) {
        let root = request.block.qualified_root();
        #[cfg(feature = "rai_protocol")]
        let root = self.request_root(&request);
        let hash = request.block.hash();
        #[cfg(feature = "rai_protocol")]
        if std::env::var_os("NANO_RAI_RECOVERY_DIAGNOSTICS").is_some() {
            tracing::info!(target: "rsnano_node::consensus::epochs::coordinator", root = %root.root, %hash, epoch = root.epoch, "RAI diagnostic election created");
        }
        let mut election = Election::new(request.block, request.behavior, self.base_latency, now);
        #[cfg(feature = "rai_protocol")]
        election.set_qualified_root(root.clone());

        self.roots.insert(Entry {
            root: root.clone(),
            election,
            priority: request.priority,
        });

        *self.count_by_behavior_mut(request.behavior) += 1;
        self.stats.started(request.behavior);
        self.notify(AecFact::ElectionStarted(hash, root));
    }

    pub fn try_add_fork(&mut self, fork: &Block, fork_tally: Amount) -> bool {
        let root = fork.qualified_root();
        #[cfg(feature = "rai_protocol")]
        let root = root.clone().with_epoch(self.epoch_for_slot(root.slot()));
        self.try_add_fork_at_root(fork, fork_tally, root)
    }

    fn try_add_fork_at_root(
        &mut self,
        fork: &Block,
        fork_tally: Amount,
        root: QualifiedRoot,
    ) -> bool {
        let Some(entry) = self.roots.get_mut(&root) else {
            return false;
        };

        let result = entry.election.try_add_fork(fork, fork_tally);
        let added = match result {
            AddForkResult::Added => {
                self.notify(AecFact::BlockAddedToElection(fork.hash()));
                true
            }
            AddForkResult::Replaced(removed) => {
                self.roots.vote_router.disconnect(
                    &removed.hash(),
                    #[cfg(feature = "rai_protocol")]
                    root.epoch,
                );
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
            self.roots.vote_router.connect(fork.hash(), root);
            self.stats.conflicts += 1;
        }

        added
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_cut_recovery(&mut self, block: Block, now: Timestamp) -> bool {
        let slot = block.qualified_root().slot();
        let Some((epoch, cut)) = &self.closing_cut else {
            return false;
        };
        if !cut.contains(&slot) {
            return false;
        }
        self.insert_vote_recovery(block, *epoch, now)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_vote_recovery(&mut self, block: Block, epoch: u64, now: Timestamp) -> bool {
        if self.stopped || self.sealed_epochs.contains(&epoch) {
            return false;
        }
        let slot = block.qualified_root().slot();
        let root = slot.with_epoch(epoch);
        // A later provisional finalization must not prevent an earlier election.
        // Membership in a sealed epoch is immutable, regardless of its number.
        if self.finalized_by_epoch.iter().any(|(finalized_epoch, values)| {
            (*finalized_epoch <= epoch || self.sealed_epochs.contains(finalized_epoch))
                && values.contains_key(&slot)
        }) {
            return false;
        }
        if self.roots.get(&root).is_some() {
            return self.try_add_fork_at_root(&block, Amount::ZERO, root);
        }

        let hash = block.hash();
        let election =
            Election::new_unsaved(block, ElectionBehavior::Hinted, self.base_latency, now);
        let mut election = election;
        election.set_qualified_root(root.clone());
        self.roots.insert(Entry {
            root: root.clone(),
            election,
            priority: BlockPriority::default(),
        });
        *self.count_by_behavior_mut(ElectionBehavior::Hinted) += 1;
        self.stats.started(ElectionBehavior::Hinted);
        self.notify(AecFact::ElectionStarted(hash, root));
        true
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
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.roots.election_for_block(block_hash)
        }
        #[cfg(feature = "rai_protocol")]
        {
            self.roots
                .election_for_block(block_hash, self.rai_epoch.current())
        }
    }

    pub fn transition_active(&mut self, block_hash: &BlockHash) -> bool {
        #[cfg(not(feature = "rai_protocol"))]
        let election = self.roots.election_for_block_mut(block_hash);
        #[cfg(feature = "rai_protocol")]
        let election = self
            .roots
            .election_for_block_mut(block_hash, self.rai_epoch.current());
        let Some(election) = election else {
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

                // RAI retains started elections until finalization or epoch exclusion.
                #[cfg(feature = "rai_protocol")]
                if bucket_vacancy <= 0 {
                    continue;
                }

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

                #[cfg(not(feature = "rai_protocol"))]
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
                    Err(AecInsertError::Stopped) => {}
                    #[cfg(feature = "rai_protocol")]
                    Err(AecInsertError::DependencyUnfinalized) => {}
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

    #[cfg(not(feature = "rai_protocol"))]
    pub fn erase_lowest_prio_election(&mut self, bucket_id: usize) {
        let Some((root, _)) = self.lowest_priority(bucket_id) else {
            return;
        };
        self.erase(&root);
    }

    fn cleanup_election(&mut self, entry: Entry) {
        let election = &entry.election;

        #[cfg(feature = "rai_protocol")]
        if election.is_confirmed() {
            self.record_finalized(
                election.qualified_root().epoch,
                election.qualified_root().slot(),
                election.winner().hash(),
            );
        }

        #[cfg(feature = "rai_protocol")]
        let superseded = if election.is_confirmed() {
            let finalized_root = election.qualified_root().clone();
            self.roots.drain_later_epochs(&finalized_root)
        } else {
            Vec::new()
        };

        // Keep track of election count by election type
        *self.count_by_behavior_mut(election.behavior()) -= 1;

        self.stats.stopped(&entry.election);
        self.notify(AecFact::ElectionEnded(entry.election));

        #[cfg(feature = "rai_protocol")]
        for entry in superseded {
            *self.count_by_behavior_mut(entry.election.behavior()) -= 1;
            self.stats.stopped(&entry.election);
            self.notify(AecFact::ElectionEnded(entry.election));
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn finalized_for_epoch(&self, epoch: u64) -> HashMap<rsnano_types::SlotRoot, BlockHash> {
        self.finalized_by_epoch
            .get(&epoch)
            .cloned()
            .unwrap_or_default()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn finalized_before_epoch(&self, epoch: u64) -> HashSet<rsnano_types::SlotRoot> {
        self.finalized_by_epoch
            .iter()
            .filter(|(candidate, _)| **candidate < epoch && self.sealed_epochs.contains(candidate))
            .flat_map(|(_, finalized)| finalized.keys().copied())
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn clear_finalized_for_epoch(&mut self, epoch: u64) {
        self.sealed_epochs.remove(&epoch);
        self.finalized_by_epoch.remove(&epoch);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn seal_finalized_epoch(&mut self, epoch: u64) {
        self.sealed_epochs.insert(epoch);
        self.received_epochs.retain(|_, received| *received != epoch);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn merge_finalized_for_epoch(
        &mut self,
        epoch: u64,
        finalized: HashMap<rsnano_types::SlotRoot, BlockHash>,
    ) {
        for (slot, hash) in finalized {
            self.record_finalized(epoch, slot, hash);
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn finalization_epoch(&self, slot: rsnano_types::SlotRoot, proposed: u64) -> u64 {
        let epochs = || self.finalized_by_epoch.iter()
            .filter_map(|(epoch, values)| values.contains_key(&slot).then_some(*epoch));
        epochs().find(|epoch| self.sealed_epochs.contains(epoch))
            .unwrap_or_else(|| epochs().min().map_or(proposed, |epoch| epoch.min(proposed)))
    }

    #[cfg(feature = "rai_protocol")]
    fn record_finalized(&mut self, epoch: u64, slot: rsnano_types::SlotRoot, hash: BlockHash) {
        if epoch == 0 || self.sealed_epochs.contains(&epoch) {
            return;
        }
        let assigned = self.finalization_epoch(slot, epoch);
        if self.sealed_epochs.contains(&assigned) {
            return;
        }
        for (other_epoch, values) in &mut self.finalized_by_epoch {
            if *other_epoch > assigned {
                values.remove(&slot);
            }
        }
        // An earlier finalization determines both the epoch and the winner.
        self.finalized_by_epoch.entry(assigned).or_default().entry(slot).or_insert(hash);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn replace_finalized_for_epoch(
        &mut self,
        epoch: u64,
        finalized: HashMap<rsnano_types::SlotRoot, BlockHash>,
    ) {
        if self.sealed_epochs.contains(&epoch) {
            return;
        }
        self.finalized_by_epoch.remove(&epoch);
        self.merge_finalized_for_epoch(epoch, finalized);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn current_epoch(&self) -> u64 {
        self.rai_epoch.current()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn advance_epoch(&self) -> u64 {
        self.rai_epoch.advance()
    }

    #[cfg(feature = "rai_protocol")]
    fn epoch_for_slot(&self, slot: rsnano_types::SlotRoot) -> u64 {
        self.closing_cut
            .as_ref()
            .filter(|(_, cut)| cut.contains(&slot))
            .map(|(epoch, _)| *epoch)
            .unwrap_or_else(|| self.rai_epoch.current())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn install_epoch_cut(
        &mut self,
        epoch: u64,
        cut: std::collections::HashSet<rsnano_types::SlotRoot>,
        now: Timestamp,
    ) -> usize {
        let mut reclassified = 0;
        self.closing_cut = Some((epoch, cut.clone()));
        for slot in cut {
            // A slot belongs to the earliest epoch whose finalized dependency closure contains
            // it. A peer may still report a later-epoch duplicate which this node already removed
            // when the earlier election confirmed; never resurrect that duplicate into a later
            // cut.
            if self
                .finalized_by_epoch
                .iter()
                .any(|(candidate, finalized)| {
                    self.sealed_epochs.contains(candidate)
                        && finalized.contains_key(&slot)
                })
            {
                continue;
            }
            // The next epoch opens while reports for this cut are in flight. A slot can finish
            // in that interval after being inserted as a later-epoch election. Once the common
            // cut includes it, move that already-finalized result back to the closing epoch just
            // as we reclassify an active later-epoch election below.
            let later_finalized = self
                .finalized_by_epoch
                .iter()
                .filter(|(candidate, _)| **candidate > epoch)
                .find_map(|(candidate, finalized)| {
                    finalized.get(&slot).copied().map(|hash| (*candidate, hash))
                });
            if let Some((_, hash)) = later_finalized {
                self.record_finalized(epoch, slot, hash);
                reclassified += 1;
                continue;
            }
            if self
                .roots
                .election_for_root(&slot.with_epoch(epoch))
                .is_some()
                || self
                    .finalized_by_epoch
                    .get(&epoch)
                    .is_some_and(|finalized| finalized.contains_key(&slot))
            {
                continue;
            }
            let later = self.roots.drain_later_epochs(&slot.with_epoch(epoch));
            let replacement = later
                .iter()
                .find_map(|entry| match entry.election.winner() {
                    MaybeSavedBlock::Saved(block) => {
                        Some((block.clone(), entry.election.behavior(), entry.priority))
                    }
                    MaybeSavedBlock::Unsaved(_) => None,
                });
            for entry in later {
                *self.count_by_behavior_mut(entry.election.behavior()) -= 1;
                self.stats.stopped(&entry.election);
                self.notify(AecFact::ElectionEnded(entry.election));
            }
            if let Some((block, behavior, priority)) = replacement {
                reclassified += 1;
                let _ = self.insert(
                    AecInsertRequest {
                        block,
                        behavior,
                        priority,
                        epoch: Some(epoch),
                    },
                    now,
                );
            }
        }
        reclassified
    }

    #[cfg(feature = "rai_protocol")]
    pub fn clear_epoch_cut(&mut self, epoch: u64) {
        if self
            .closing_cut
            .as_ref()
            .is_some_and(|(closing, _)| *closing == epoch)
        {
            self.closing_cut = None;
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn remove_epoch_elections(&mut self, epoch: u64) {
        let removed = self.roots.drain_filter(|entry| entry.root.epoch == epoch);
        for entry in removed {
            *self.count_by_behavior_mut(entry.election.behavior()) -= 1;
            self.stats.stopped(&entry.election);
            self.notify(AecFact::ElectionEnded(entry.election));
        }
    }

    /// Dependent elections are implicitly confirmed when their block is confirmed
    pub fn confirm_dependent_elections(
        &mut self,
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>, u64)>,
        now: Timestamp,
    ) {
        for (confirmed_block, source_election, source_epoch) in confirmed {
            #[cfg(feature = "rai_protocol")]
            if std::env::var_os("NANO_RAI_RECOVERY_DIAGNOSTICS").is_some() {
                tracing::info!(target: "rsnano_node::consensus::epochs::coordinator", root = %confirmed_block.root(), hash = %confirmed_block.hash(), source_epoch, source = ?source_election.as_ref().map(|e| e.winner.hash()), "RAI diagnostic cementation input");
            }
            let confirmed_election = self.confirm_dependent_election(
                &confirmed_block,
                source_election,
                source_epoch,
                now,
            );

            #[cfg(feature = "rai_protocol")]
            for mut entry in self.roots.drain_filter(|entry| {
                entry.root.slot() == confirmed_block.qualified_root().slot()
            }) {
                if entry.election.winner().hash() == confirmed_block.hash() {
                    entry.election.force_confirm();
                } else {
                    entry.election.cancel();
                }
                *self.count_by_behavior_mut(entry.election.behavior()) -= 1;
                self.stats.stopped(&entry.election);
                self.notify(AecFact::ElectionEnded(entry.election));
            }

            self.block_confirmed(confirmed_block, confirmed_election);
        }
    }

    fn confirm_dependent_election(
        &mut self,
        confirmed_block: &SavedBlock,
        source_election: Option<ConfirmedElection>,
        source_epoch: u64,
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

        #[cfg(not(feature = "rai_protocol"))]
        let corresponding = self.roots.get_mut(&confirmed_block.qualified_root());
        #[cfg(feature = "rai_protocol")]
        let corresponding = self
            .roots
            .election_for_block_any_epoch_mut(&confirmed_block.hash())
            .map(|election| election);

        let Some(corresponding_election) = corresponding else {
            let mut result = ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::InactiveConfirmationHeight,
            );
            #[cfg(feature = "rai_protocol")]
            {
                result.epoch = source_epoch;
            }
            return result;
        };

        #[cfg(not(feature = "rai_protocol"))]
        let corresponding_election = &mut corresponding_election.election;

        let result = if corresponding_election.winner().hash() == confirmed_block.hash() {
            corresponding_election.force_confirm();
            corresponding_election
                .into_confirmed_election(now, ConfirmationType::ActiveConfirmationHeight)
        } else {
            #[cfg(feature = "rai_protocol")]
            let epoch = corresponding_election.qualified_root().epoch;
            corresponding_election.cancel();
            let result = ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::ActiveConfirmationHeight,
            );
            #[cfg(feature = "rai_protocol")]
            let result = {
                let mut result = result;
                result.epoch = epoch;
                result
            };
            result
        };
        #[cfg(feature = "rai_protocol")]
        let result = {
            let mut result = result;
            // An active election's epoch is not a finalization. Dependencies inherit
            // the source's epoch unless an actual earlier finalization already exists.
            if source_epoch > 0 {
                result.epoch = source_epoch;
            }
            result
        };
        result
    }

    fn block_confirmed(&mut self, block: SavedBlock, election: ConfirmedElection) {
        #[cfg(feature = "rai_protocol")]
        let election = {
            let mut election = election;
            if election.epoch > 0 {
                election.epoch = self.finalization_epoch(block.qualified_root().slot(), election.epoch);
            }
            election
        };
        #[cfg(feature = "rai_protocol")]
        if std::env::var_os("NANO_RAI_RECOVERY_DIAGNOSTICS").is_some() {
            tracing::info!(target: "rsnano_node::consensus::epochs::coordinator", root = %block.root(), hash = %block.hash(), epoch = election.epoch, kind = ?election.confirmation_type, "RAI diagnostic cementation attribution");
        }
        #[cfg(feature = "rai_protocol")]
        self.record_finalized(election.epoch, block.qualified_root().slot(), block.hash());
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
            self.cleanup_election(entry);
        }
        result.per_block
    }

    pub fn force_confirm(&mut self, block_hash: &BlockHash, now: Timestamp) {
        #[cfg(not(feature = "rai_protocol"))]
        let election = self.roots.election_for_block_mut(block_hash);
        #[cfg(feature = "rai_protocol")]
        let election = self
            .roots
            .election_for_block_mut(block_hash, self.rai_epoch.current());
        let Some(election) = election else {
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
    #[cfg(feature = "rai_protocol")]
    use rsnano_types::VoteType;
    use crate::consensus::ReceivedVote;
    use rsnano_types::{BlockPriority, PrivateKey, TimePriority, Vote, VoteDelivery};
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
            #[cfg(feature = "rai_protocol")]
            epoch: None,
        };

        container
            .insert(request, Timestamp::new_test_instance())
            .unwrap();

        assert_eq!(container.len(), 1);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn earlier_epoch_prevents_later_election_for_same_slot() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let epoch_one_root = block.qualified_root().with_epoch(1);
        let epoch_two_root = block.qualified_root().with_epoch(2);
        let request = || AecInsertRequest {
            block: block.clone(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
            epoch: None,
        };

        container
            .insert(request(), Timestamp::new_test_instance())
            .unwrap();
        container.advance_epoch();
        assert_eq!(
            container.insert(request(), Timestamp::new_test_instance()),
            Err(AecInsertError::Duplicate)
        );
        assert!(container.is_active_root(&epoch_one_root));
        assert!(!container.is_active_root(&epoch_two_root));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn late_cut_block_is_inserted_into_closing_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let slot = block.qualified_root().slot();
        container.advance_epoch();
        container.install_epoch_cut(1, [slot].into(), Timestamp::new_test_instance());

        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                    epoch: None,
                },
                Timestamp::new_test_instance(),
            )
            .unwrap();

        assert!(container.is_active_root(&slot.with_epoch(1)));
        assert!(!container.is_active_root(&slot.with_epoch(2)));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn recovery_publish_cannot_remove_finalized_value() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let slot = block.qualified_root().slot();
        let finalized_hash = BlockHash::from(999);
        container.merge_finalized_for_epoch(1, [(slot, finalized_hash)].into());
        container.install_epoch_cut(1, [slot].into(), Timestamp::new_test_instance());
        assert!(!container.insert_cut_recovery(block.into(), Timestamp::new_test_instance()));
        assert_eq!(container.finalized_for_epoch(1).get(&slot), Some(&finalized_hash));
        assert!(!container.is_active_root(&slot.with_epoch(1)));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn hinted_recovery_can_restore_non_cut_election_in_closing_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let slot = block.qualified_root().slot();
        container.advance_epoch();
        container.install_epoch_cut(1, Default::default(), Timestamp::new_test_instance());

        container
            .insert(
                AecInsertRequest::new_hinted_for_epoch(
                    block,
                    BlockPriority::new_test_instance(),
                    1,
                ),
                Timestamp::new_test_instance(),
            )
            .unwrap();

        assert!(container.is_active_root(&slot.with_epoch(1)));
        assert!(!container.is_active_root(&slot.with_epoch(2)));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn cut_restarts_existing_later_epoch_election_in_closing_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let slot = block.qualified_root().slot();
        container.advance_epoch();
        container
            .insert(
                AecInsertRequest {
                    block,
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::new_test_instance(),
                    epoch: None,
                },
                Timestamp::new_test_instance(),
            )
            .unwrap();
        assert!(container.is_active_root(&slot.with_epoch(2)));

        container.install_epoch_cut(1, [slot].into(), Timestamp::new_test_instance());

        assert!(container.is_active_root(&slot.with_epoch(1)));
        assert!(!container.is_active_root(&slot.with_epoch(2)));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn cut_reclassifies_slot_finalized_while_next_epoch_was_open() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let slot = block.qualified_root().slot();
        container.merge_finalized_for_epoch(2, [(slot, block.hash())].into());

        assert_eq!(
            container.install_epoch_cut(1, [slot].into(), Timestamp::new_test_instance()),
            1
        );
        assert_eq!(
            container.finalized_for_epoch(1).get(&slot),
            Some(&block.hash())
        );
        assert!(!container.finalized_for_epoch(2).contains_key(&slot));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn earlier_child_confirmation_assigns_dependency_to_earlier_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let slot = block.qualified_root().slot();
        let now = Timestamp::new_test_instance();
        container.advance_epoch();
        container.insert(AecInsertRequest {
            block: block.clone(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
            epoch: None,
        }, now).unwrap();
        container.confirm_dependent_elections(vec![(block.clone(), None, 1)], now);
        assert_eq!(container.finalized_for_epoch(1).get(&slot), Some(&block.hash()));
        assert!(container.finalized_for_epoch(2).is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn validated_vote_recovers_non_cut_election_without_changing_cut() {
        let mut container = ActiveElectionsContainer::default();
        let block: Block = SavedBlock::new_test_instance().into();
        let slot = block.qualified_root().slot();
        let now = Timestamp::new_test_instance();
        container.install_epoch_cut(1, HashSet::new(), now);
        assert!(!container.insert_cut_recovery(block.clone(), now));
        assert!(container.insert_vote_recovery(block.clone(), 2, now));
        assert!(container.insert_vote_recovery(block, 1, now));
        assert!(container.is_active_root(&slot.with_epoch(1)));
        assert!(container.closing_cut.as_ref().unwrap().1.is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn vote_starts_election_without_a_cut_and_accepts_its_vote() {
        let mut container = ActiveElectionsContainer::default();
        let block: Block = SavedBlock::new_test_instance().into();
        let hash = block.hash();
        let root = block.qualified_root().with_epoch(2);
        let now = Timestamp::new_test_instance();
        assert!(container.insert_vote_recovery(block, 2, now));
        let rep = PrivateKey::from(1);
        let vote: crate::consensus::FilteredVote = ReceivedVote::new(
            Arc::new(Vote::new_rai(&rep, 2, VoteType::First, vec![hash])),
            VoteDelivery::Direct, None,
        ).into();
        let weights = Default::default();
        let quorum = crate::representatives::QuorumSnapshot::new_test_instance();
        let results = container.apply_vote(ApplyVoteArgs {
            vote: &vote, rep_weights: &weights, quorum_snapshot: &quorum, now,
        });
        assert_eq!(results.get(&hash), Some(&Ok(())));
        assert_eq!(container.election_for_root(&root).unwrap().vote_count(), 1);
        assert!(container.closing_cut.is_none());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn earlier_vote_can_start_after_later_provisional_finalization() {
        let mut container = ActiveElectionsContainer::default();
        let block: Block = SavedBlock::new_test_instance().into();
        let slot = block.qualified_root().slot();
        let now = Timestamp::new_test_instance();
        container.merge_finalized_for_epoch(2, [(slot, block.hash())].into());
        assert!(!container.insert_vote_recovery(block.clone(), 3, now));
        assert!(!container.insert_vote_recovery(block.clone(), 2, now));
        assert!(container.insert_vote_recovery(block, 1, now));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn sealed_membership_prevents_earlier_vote_recovery_and_reassignment() {
        let mut container = ActiveElectionsContainer::default();
        let block: Block = SavedBlock::new_test_instance().into();
        let slot = block.qualified_root().slot();
        container.merge_finalized_for_epoch(2, [(slot, block.hash())].into());
        container.seal_finalized_epoch(2);
        assert!(!container.insert_vote_recovery(block.clone(), 1, Timestamp::new_test_instance()));
        container.merge_finalized_for_epoch(1, [(slot, block.hash())].into());
        assert!(container.finalized_for_epoch(1).is_empty());
        assert_eq!(container.finalized_for_epoch(2).get(&slot), Some(&block.hash()));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn earlier_dependency_finalization_replaces_later_assignment() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let slot = block.qualified_root().slot();
        let now = Timestamp::new_test_instance();
        container.confirm_dependent_elections(vec![(block.clone(), None, 2)], now);
        container.confirm_dependent_elections(vec![(block.clone(), None, 1)], now);
        container.confirm_dependent_elections(vec![(block.clone(), None, 3)], now);
        container.erase_ended_elections();
        assert_eq!(container.finalized_for_epoch(1).get(&slot), Some(&block.hash()));
        assert!(container.finalized_for_epoch(2).is_empty());
        assert!(container.finalized_for_epoch(3).is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn full_priority_bucket_retains_its_election_and_queued_candidate() {
        struct Source(Option<crate::consensus::ElectionCandidate>);
        impl ElectionCandidateSource for Source {
            fn should_schedule(&self, _: &[crate::consensus::BucketInfo]) -> bool { self.0.is_some() }
            fn next_candidate(&mut self, bucket: usize, _: isize, _: TimePriority)
                -> Option<crate::consensus::ElectionCandidate>
            {
                if self.0.as_ref().is_some_and(|candidate| candidate.bucket_id == bucket) {
                    self.0.take()
                } else { None }
            }
        }
        let mut container = ActiveElectionsContainer::default();
        container.max_elections_per_bucket = 1;
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root().with_epoch(1);
        let priority = BlockPriority::new_test_instance();
        let now = Timestamp::new_test_instance();
        container.insert(AecInsertRequest::new_priority(block, priority), now).unwrap();
        let mut source = Source(Some(crate::consensus::ElectionCandidate {
            bucket_id: container.find_bucket(&root).unwrap(),
            block: SavedBlock::new_test_instance_with_key(2), priority,
        }));
        container.refill(&mut source, now);
        assert!(container.is_active_root(&root));
        assert_eq!(container.len(), 1);
        assert!(source.0.is_some());

        let queued = source.0.as_ref().unwrap().block.clone();
        container.observe_block_receipt(queued.hash());
        container.advance_epoch();
        container.observe_block_receipt(queued.hash());
        container.erase(&root);
        container.refill(&mut source, now);
        assert!(source.0.is_none());
        assert!(container.is_active_root(&queued.qualified_root().with_epoch(1)));
        assert!(!container.is_active_root(&queued.qualified_root().with_epoch(2)));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn receipt_epoch_survives_provisional_cut_but_not_decided_exclusion() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        let request = AecInsertRequest::new_priority(block.clone(), BlockPriority::default());
        container.observe_block_receipt(block.hash());
        container.advance_epoch();
        container.install_epoch_cut(1, HashSet::new(), now);
        assert_eq!(container.request_root(&request).epoch, 1);
        container.mark_epoch_cut_decided(1);
        assert_eq!(container.request_root(&request).epoch, 2);

        let fresh = SavedBlock::new_test_instance_with_key(3);
        container.observe_block_receipt(fresh.hash());
        let fresh_request = AecInsertRequest::new_priority(fresh, BlockPriority::default());
        assert_eq!(container.request_root(&fresh_request).epoch, 2);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn decided_cut_inclusion_and_explicit_vote_epoch_override_receipt() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        container.observe_block_receipt(block.hash());
        container.advance_epoch();
        container.install_epoch_cut(1, [block.qualified_root().slot()].into(), now);
        container.mark_epoch_cut_decided(1);
        let mut request = AecInsertRequest::new_priority(block, BlockPriority::default());
        assert_eq!(container.request_root(&request).epoch, 1);
        request.epoch = Some(2);
        assert_eq!(container.request_root(&request).epoch, 2);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn recovery_publish_recreates_missing_cut_election_from_unsaved_block() {
        let mut container = ActiveElectionsContainer::default();
        let saved = SavedBlock::new_test_instance();
        let block: Block = saved.clone().into();
        let slot = block.qualified_root().slot();
        container.install_epoch_cut(1, [slot].into(), Timestamp::new_test_instance());

        assert!(container.insert_cut_recovery(block, Timestamp::new_test_instance()));

        let election = container
            .election_for_root(&slot.with_epoch(1))
            .expect("recovery election");
        assert_eq!(election.winner().hash(), saved.hash());
        assert!(matches!(election.winner(), MaybeSavedBlock::Unsaved(_)));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn later_cut_does_not_resurrect_slot_finalized_in_earlier_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let slot = block.qualified_root().slot();
        container.merge_finalized_for_epoch(1, [(slot, block.hash())].into());
        container.seal_finalized_epoch(1);
        container.advance_epoch();

        assert_eq!(
            container.install_epoch_cut(2, [slot].into(), Timestamp::new_test_instance()),
            0
        );
        assert!(!container.is_active_root(&slot.with_epoch(2)));
        assert!(container.finalized_for_epoch(2).is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn indirectly_confirmed_dependency_inherits_source_election_epoch() {
        let mut container = ActiveElectionsContainer::default();
        container.advance_epoch();
        let dependent = SavedBlock::new_test_instance_with_key(1);
        let source_block = SavedBlock::new_test_instance_with_key(2);
        let mut source =
            ConfirmedElection::new(source_block, ConfirmationType::ActiveConfirmedQuorum);
        source.epoch = 1;

        container.confirm_dependent_elections(
            vec![(dependent.clone(), Some(source), 1)],
            Timestamp::new_test_instance(),
        );

        assert_eq!(
            container
                .finalized_for_epoch(1)
                .get(&dependent.qualified_root().slot()),
            Some(&dependent.hash())
        );
        assert!(container.finalized_for_epoch(2).is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn indirectly_confirmed_dependency_keeps_entry_epoch_without_cached_election() {
        let mut container = ActiveElectionsContainer::default();
        let dependent = SavedBlock::new_test_instance_with_key(3);

        container.confirm_dependent_elections(
            vec![(dependent.clone(), None, 1)],
            Timestamp::new_test_instance(),
        );

        assert_eq!(
            container
                .finalized_for_epoch(1)
                .get(&dependent.qualified_root().slot()),
            Some(&dependent.hash())
        );
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn indirectly_confirmed_active_dependency_inherits_finalizing_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let dependent = SavedBlock::new_test_instance_with_key(4);
        let slot = dependent.qualified_root().slot();
        container
            .insert(
                AecInsertRequest::new_hinted_for_epoch(
                    dependent.clone(),
                    BlockPriority::new_test_instance(),
                    1,
                ),
                Timestamp::new_test_instance(),
            )
            .unwrap();
        let source_block = SavedBlock::new_test_instance_with_key(5);
        let mut source =
            ConfirmedElection::new(source_block, ConfirmationType::ActiveConfirmedQuorum);
        source.epoch = 2;

        container.confirm_dependent_elections(
            vec![(dependent.clone(), Some(source), 2)],
            Timestamp::new_test_instance(),
        );

        assert_eq!(
            container.finalized_for_epoch(2).get(&slot),
            Some(&dependent.hash())
        );
        assert!(!container.finalized_for_epoch(1).contains_key(&slot));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn cemented_alternate_candidate_inherits_finalizing_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let key = PrivateKey::from(1);
        let make_block = |balance| SavedBlock::new_test_instance_with(rsnano_types::StateBlockArgs {
            key: &key, previous: BlockHash::from(1), representative: 789.into(),
            balance: Amount::from(balance), link: 111.into(), work: 69420.into(),
        }.into());
        let local = make_block(100u128);
        let cemented = make_block(200u128);
        let slot = local.qualified_root().slot();
        container.insert(AecInsertRequest::new_hinted_for_epoch(
            local, BlockPriority::new_test_instance(), 1,
        ), Timestamp::new_test_instance()).unwrap();
        assert!(container.try_add_fork(&cemented.clone().into(), Amount::ZERO));
        container.confirm_dependent_elections(vec![(cemented.clone(), None, 2)], Timestamp::new_test_instance());
        assert_eq!(container.finalized_for_epoch(2).get(&slot), Some(&cemented.hash()));
        assert!(container.finalized_for_epoch(1).is_empty());
    }

    #[test]
    fn confirm_election() {
        let mut container = ActiveElectionsContainer::default();

        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();

        let request = AecInsertRequest {
            block,
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
            #[cfg(feature = "rai_protocol")]
            epoch: None,
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
            #[cfg(feature = "rai_protocol")]
            epoch: None,
        };

        let start = Timestamp::new_test_instance();

        container.insert(request, start).unwrap();

        assert_eq!(container.info(start).stale, 0);
        assert_eq!(container.info(start + Duration::from_secs(60)).stale, 1);
    }

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
