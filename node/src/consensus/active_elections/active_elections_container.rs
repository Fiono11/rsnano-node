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
    rai_epoch: RaiEpoch,
    #[cfg(feature = "rai_protocol")]
    rai_finalized: HashMap<QualifiedRoot, BlockHash>,
    #[cfg(feature = "rai_protocol")]
    rai_terminated: HashSet<QualifiedRoot>,
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
            rai_finalized: HashMap::new(),
            #[cfg(feature = "rai_protocol")]
            rai_terminated: HashSet::new(),
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

        let root = request.block.qualified_root();
        #[cfg(feature = "rai_protocol")]
        let root = root.with_epoch(self.rai_epoch.current());
        if self.try_upgrade_priority_election(&request, root)? {
            return Ok(());
        }

        self.insert_new_election(request, now);
        Ok(())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_for_epoch(
        &mut self,
        request: AecInsertRequest,
        now: Timestamp,
        epoch: u64,
    ) -> Result<(), AecInsertError> {
        self.ensure_not_stopped()?;
        self.ensure_not_recently_confirmed(&request)?;
        let root = request.block.qualified_root().with_epoch(epoch);
        if self.try_upgrade_priority_election(&request, root.clone())? {
            return Ok(());
        }
        let mut election = Election::new(request.block, request.behavior, self.base_latency, now);
        election.set_qualified_root(root.clone());
        let hash = election.winner().hash();
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

    /// Returns whether the certified epoch already has an independent election.
    /// Elections in other epochs deliberately remain untouched.
    #[cfg(feature = "rai_protocol")]
    pub fn has_election_for_epoch(&self, hash: &BlockHash, epoch: u64) -> bool {
        self.roots
            .vote_router
            .qualified_root_for_epoch(hash, epoch)
            .is_some()
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
        if self.recently_confirmed.hash_exists(&request.block.hash()) {
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

    fn insert_new_election(&mut self, request: AecInsertRequest, now: Timestamp) {
        let root = request.block.qualified_root();
        #[cfg(feature = "rai_protocol")]
        let root = root.with_epoch(self.rai_epoch.current());
        let hash = request.block.hash();
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
        let root = root.with_epoch(self.rai_epoch.current());
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
                #[cfg(not(feature = "rai_protocol"))]
                self.roots.vote_router.disconnect(&removed.hash());
                #[cfg(feature = "rai_protocol")]
                self.roots
                    .vote_router
                    .disconnect_for_epoch(&removed.hash(), root.epoch);
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

    #[cfg(feature = "rai_protocol")]
    pub fn epoch_slots(&self, epoch: u64) -> Vec<(QualifiedRoot, BlockHash)> {
        self.roots.epoch_slots(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn finalized_epoch_slots(&self, epoch: u64) -> Vec<(QualifiedRoot, BlockHash)> {
        self.rai_finalized
            .iter()
            .filter(|(root, _)| root.epoch == epoch)
            .map(|(root, hash)| (root.clone(), *hash))
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn epoch_slot_outcome(&self, root: &QualifiedRoot) -> Option<Option<BlockHash>> {
        if let Some(hash) = self.rai_finalized.get(root) {
            Some(Some(*hash))
        } else if self.rai_terminated.contains(root) {
            Some(None)
        } else if let Some(election) = self.roots.get(root).map(|entry| &entry.election)
            && election.is_terminated()
        {
            if election.terminated_by_timeout() {
                Some(None)
            } else {
                Some(Some(election.winner().hash()))
            }
        } else {
            None
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn epoch_slot_finalized_or_timed_out(&self, root: &QualifiedRoot) -> bool {
        if self.rai_finalized.contains_key(root) || self.rai_terminated.contains(root) {
            return true;
        }
        self.roots.get(root).is_some_and(|entry| {
            entry.election.is_confirmed() || entry.election.terminated_by_timeout()
        })
    }

    #[cfg(feature = "rai_protocol")]
    pub fn begin_epoch_one(&mut self) {
        debug_assert_eq!(self.rai_epoch.current(), 1);
        self.rai_finalized.clear();
        self.rai_terminated.clear();
    }

    #[cfg(feature = "rai_protocol")]
    pub fn advance_epoch(&self) -> u64 {
        self.rai_epoch.advance()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn exclude_by_cut(&mut self, root: &QualifiedRoot) -> bool {
        if let Some(entry) = self.roots.get_mut(root) {
            // A cut exclusion stops fresh local support, but the election must
            // remain routable for epoch votes which were already produced.
            entry.election.suppress_vote_generation();
            true
        } else {
            false
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn suppress_epoch_votes(&mut self, epoch: u64) {
        for entry in self.roots.iter_mut() {
            if entry.root.epoch == epoch {
                entry.election.suppress_vote_generation();
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn resume_cut_votes(&mut self, epoch: u64, included: &HashSet<QualifiedRoot>) {
        for entry in self.roots.iter_mut() {
            if entry.root.epoch != epoch {
                continue;
            }
            if included.contains(&entry.root) {
                entry.election.resume_vote_generation();
            } else {
                entry.election.suppress_vote_generation();
            }
        }
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

    #[cfg(feature = "rai_protocol")]
    pub fn transition_active_root(&mut self, root: &QualifiedRoot) -> bool {
        let Some(entry) = self.roots.get_mut(root) else {
            return false;
        };
        entry.election.transition_active();
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
                    #[cfg(feature = "rai_protocol")]
                    {
                        // RAI elections may only leave the AEC with terminal certificate
                        // evidence (or an explicit certified-cut exclusion). Apply
                        // backpressure instead of replacing a live election.
                        return;
                    }
                    #[cfg(not(feature = "rai_protocol"))]
                    {
                        self.erase_lowest_prio_election(candidate.bucket_id);
                        self.stats.replaced += 1;
                    }
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
        #[cfg(feature = "rai_protocol")]
        return;

        #[cfg(not(feature = "rai_protocol"))]
        let removed = self.roots.drain_filter(|i| i.election.state().has_ended());

        #[cfg(not(feature = "rai_protocol"))]
        for entry in removed {
            self.cleanup_election(entry);
        }
    }

    pub fn erase(&mut self, root: &QualifiedRoot) -> bool {
        #[cfg(feature = "rai_protocol")]
        {
            // RAI elections may only be removed by one of the certified terminal
            // paths below. Generic cleanup must not discard unresolved evidence.
            let _ = root;
            return false;
        }
        #[cfg(not(feature = "rai_protocol"))]
        self.erase_certified(root)
    }

    fn erase_certified(&mut self, root: &QualifiedRoot) -> bool {
        let Some(entry) = self.roots.erase(root) else {
            return false;
        };
        self.cleanup_election(entry);
        true
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_cemented_outcome(&mut self, block: &SavedBlock) -> bool {
        let Some(root) = self
            .roots
            .vote_router
            .qualified_roots(&block.hash())
            .into_iter()
            .find(|root| {
                self.roots.get(root).is_some_and(|entry| {
                    entry.election.is_confirmed() && entry.election.winner().hash() == block.hash()
                })
            })
        else {
            return false;
        };
        self.rai_finalized.insert(root.clone(), block.hash());
        self.rai_terminated.remove(&root);
        let competing = self.roots.roots_for_slot(&root);
        let mut erased = false;
        for candidate in competing {
            if candidate != root {
                self.rai_terminated.insert(candidate.clone());
            }
            erased |= self.erase_certified(&candidate);
        }
        erased
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_rolled_back_outcome(&mut self, root: &QualifiedRoot) -> bool {
        self.rai_terminated.insert(root.clone());
        self.erase_certified(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_record_outcome(&mut self, root: &QualifiedRoot, hash: BlockHash) -> bool {
        self.rai_finalized.insert(root.clone(), hash);
        self.rai_terminated.remove(root);
        let competing = self.roots.roots_for_slot(root);
        let mut erased = false;
        for candidate in competing {
            if candidate != *root {
                self.rai_terminated.insert(candidate.clone());
            }
            erased |= self.erase_certified(&candidate);
        }
        erased
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_rolled_back_block(&mut self, hash: &BlockHash) -> bool {
        let roots = self.roots.vote_router.qualified_roots(hash);
        roots.into_iter().fold(false, |removed, root| {
            self.erase_certified(&root) || removed
        })
    }

    pub fn erase_lowest_prio_election(&mut self, bucket_id: usize) {
        let Some((root, _)) = self.lowest_priority(bucket_id) else {
            return;
        };
        self.erase(&root);
    }

    fn cleanup_election(&mut self, entry: Entry) {
        let election = &entry.election;

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

        #[cfg(not(feature = "rai_protocol"))]
        let corresponding = self.roots.get_mut(&confirmed_block.qualified_root());
        #[cfg(feature = "rai_protocol")]
        let corresponding = self
            .roots
            .election_for_block_mut(&confirmed_block.hash(), self.rai_epoch.current());

        let Some(corresponding_election) = corresponding else {
            return ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::InactiveConfirmationHeight,
            );
        };

        #[cfg(not(feature = "rai_protocol"))]
        let corresponding_election = &mut corresponding_election.election;

        if corresponding_election.winner().hash() == confirmed_block.hash() {
            corresponding_election.force_confirm();
            corresponding_election
                .into_confirmed_election(now, ConfirmationType::ActiveConfirmationHeight)
        } else {
            corresponding_election.cancel();
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
            self.rai_finalized
                .insert(entry.root.clone(), entry.election.winner().hash());
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
    use crate::consensus::ReceivedVote;
    use rsnano_types::{BlockPriority, PrivateKey, TimePriority, Vote, VoteDelivery, VoteType};
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

    #[test]
    fn confirm_election() {
        let mut container = ActiveElectionsContainer::default();

        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();

        let request = AecInsertRequest {
            block: block.clone(),
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

        #[cfg(not(feature = "rai_protocol"))]
        assert!(container.election_for_block(&block_hash).is_none());
        #[cfg(feature = "rai_protocol")]
        {
            assert!(container.election_for_block(&block_hash).is_some());
            assert!(container.apply_cemented_outcome(&block));
            assert!(container.election_for_block(&block_hash).is_none());
        }
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn generic_erase_cannot_remove_an_unresolved_election() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root().with_epoch(1);
        container
            .insert(
                AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                Timestamp::new_test_instance(),
            )
            .unwrap();

        assert!(!container.erase(&root));
        assert!(container.is_active_root(&root));
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn notarized_value_is_an_epoch_slot_outcome_before_finalization() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let root = block.qualified_root().with_epoch(1);
        container
            .insert(
                AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                Timestamp::new_test_instance(),
            )
            .unwrap();

        let quorum = QuorumSnapshot::new_test_instance();
        let certificate = quorum.total_weight - quorum.faulty_weight - quorum.slack_weight;
        let rep = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(rep.public_key(), certificate);
        let vote: FilteredVote = ReceivedVote::new(
            Arc::new(Vote::new_rai(&rep, 1, VoteType::First, vec![hash])),
            VoteDelivery::Direct,
            None,
        )
        .into();
        container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &weights,
            quorum_snapshot: &quorum,
            now: Timestamp::new_test_instance(),
        });

        assert_eq!(container.epoch_slot_outcome(&root), Some(Some(hash)));
        assert!(!container.epoch_slot_finalized_or_timed_out(&root));
        assert!(!container.election_for_root(&root).unwrap().is_confirmed());
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn notarized_timeout_is_a_discarded_epoch_slot_outcome() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let root = block.qualified_root().with_epoch(1);
        container
            .insert(
                AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                Timestamp::new_test_instance(),
            )
            .unwrap();

        let quorum = QuorumSnapshot::new_test_instance();
        let certificate = quorum.total_weight - quorum.faulty_weight - quorum.slack_weight;
        let rep = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(rep.public_key(), certificate);
        let vote: FilteredVote = ReceivedVote::new(
            Arc::new(Vote::new_rai(&rep, 1, VoteType::Timeout, vec![hash])),
            VoteDelivery::Direct,
            None,
        )
        .into();
        container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &weights,
            quorum_snapshot: &quorum,
            now: Timestamp::new_test_instance(),
        });

        assert_eq!(container.epoch_slot_outcome(&root), Some(None));
        assert!(container.epoch_slot_finalized_or_timed_out(&root));
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn cut_exclusion_suppresses_fresh_votes_without_erasing_the_election() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root().with_epoch(1);
        container
            .insert(
                AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                Timestamp::new_test_instance(),
            )
            .unwrap();

        assert!(container.exclude_by_cut(&root));
        let election = container.election_for_root(&root).unwrap();
        assert!(!election.vote_generation_enabled());
        assert!(container.is_active_root(&root));
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn epoch_close_suppresses_all_slots_and_cut_resumes_only_included_slots() {
        let mut container = ActiveElectionsContainer::default();
        let included = SavedBlock::new_test_instance_with_key(1);
        let excluded = SavedBlock::new_test_instance_with_key(2);
        let included_root = included.qualified_root().with_epoch(1);
        let excluded_root = excluded.qualified_root().with_epoch(1);
        for block in [included, excluded] {
            container
                .insert(
                    AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                    Timestamp::new_test_instance(),
                )
                .unwrap();
        }

        container.suppress_epoch_votes(1);
        assert!(
            !container
                .election_for_root(&included_root)
                .unwrap()
                .vote_generation_enabled()
        );
        assert!(
            !container
                .election_for_root(&excluded_root)
                .unwrap()
                .vote_generation_enabled()
        );

        container.resume_cut_votes(1, &HashSet::from([included_root.clone()]));
        assert!(
            container
                .election_for_root(&included_root)
                .unwrap()
                .vote_generation_enabled()
        );
        assert!(
            !container
                .election_for_root(&excluded_root)
                .unwrap()
                .vote_generation_enabled()
        );
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn insert_for_epoch_tracks_behavior_count_until_cleanup() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root().with_epoch(1);

        container
            .insert_for_epoch(
                AecInsertRequest::new_manual(block, BlockPriority::new_test_instance()),
                Timestamp::new_test_instance(),
                1,
            )
            .unwrap();

        assert_eq!(container.count_by_behavior(ElectionBehavior::Manual), 1);
        assert!(container.erase_certified(&root));
        assert_eq!(container.count_by_behavior(ElectionBehavior::Manual), 0);
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn same_hash_can_have_independent_elections_in_different_epochs() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let epoch_one_root = block.qualified_root().with_epoch(1);
        let now = Timestamp::new_test_instance();

        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        assert_eq!(container.advance_epoch(), 2);

        let result = container.insert(
            AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
            now,
        );

        assert_eq!(result, Ok(()));
        assert_eq!(container.len(), 2);
        assert_eq!(
            container
                .roots
                .vote_router
                .qualified_root_for_epoch(&hash, 1),
            Some(&epoch_one_root)
        );
        assert!(
            container
                .roots
                .vote_router
                .qualified_root_for_epoch(&hash, 2)
                .is_some()
        );

        let rep = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(rep.public_key(), Amount::raw(1));
        let vote: FilteredVote = ReceivedVote::new(
            Arc::new(Vote::new_rai(&rep, 1, VoteType::First, vec![hash])),
            VoteDelivery::Direct,
            None,
        )
        .into();
        container.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        let epoch_two_root = block.qualified_root().with_epoch(2);
        assert_eq!(
            container
                .election_for_root(&epoch_one_root)
                .unwrap()
                .votes()
                .len(),
            1
        );
        assert!(
            container
                .election_for_root(&epoch_two_root)
                .unwrap()
                .votes()
                .is_empty()
        );
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn recently_confirmed_hash_cannot_be_reinserted_in_another_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        container
            .recently_confirmed
            .put(block.qualified_root().with_epoch(1), hash);
        assert_eq!(container.advance_epoch(), 2);

        let result = container.insert(
            AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
            Timestamp::new_test_instance(),
        );

        assert_eq!(result, Err(AecInsertError::RecentlyConfirmed));
        assert!(container.is_empty());
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn certified_cut_does_not_reassign_another_epochs_election() {
        let mut container = ActiveElectionsContainer::default();
        assert_eq!(container.advance_epoch(), 2);
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let epoch_one_root = block.qualified_root().with_epoch(1);

        container
            .insert(
                AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                Timestamp::new_test_instance(),
            )
            .unwrap();

        assert!(!container.has_election_for_epoch(&hash, 1));
        assert_eq!(container.len(), 1);
        assert!(!container.is_active_root(&epoch_one_root));
        assert!(
            container
                .roots
                .vote_router
                .qualified_root_for_epoch(&hash, 2)
                .is_some()
        );
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
