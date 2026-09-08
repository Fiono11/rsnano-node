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
    pub epoch_source: Option<std::sync::Arc<rsnano_ledger::Ledger>>,
    insertion_epoch: Option<u64>,
    #[cfg(feature = "rai_protocol")]
    completed_ids:
        crate::consensus::bounded_hash_map::BoundedHashMap<rsnano_types::ElectionId, BlockHash>,
    #[cfg(feature = "rai_protocol")]
    confirmed_epochs:
        crate::consensus::bounded_hash_map::BoundedHashMap<QualifiedRoot, (BlockHash, u64)>,
    #[cfg(feature = "rai_protocol")]
    confirmed_epoch_by_hash: crate::consensus::bounded_hash_map::BoundedHashMap<BlockHash, u64>,
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
}

impl ActiveElectionsContainer {
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            epoch_source: None,
            insertion_epoch: None,
            #[cfg(feature = "rai_protocol")]
            completed_ids: crate::consensus::bounded_hash_map::BoundedHashMap::new(
                config.confirmation_cache,
            ),
            #[cfg(feature = "rai_protocol")]
            confirmed_epochs: crate::consensus::bounded_hash_map::BoundedHashMap::new(
                config.confirmation_cache,
            ),
            #[cfg(feature = "rai_protocol")]
            confirmed_epoch_by_hash: crate::consensus::bounded_hash_map::BoundedHashMap::new(
                config.confirmation_cache,
            ),
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

        if self.try_upgrade_priority_election(&request)? {
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
        {
            let epoch = self.insertion_epoch.unwrap_or_else(|| self.current_epoch());
            if self
                .completed_ids
                .contains_key(&rsnano_types::ElectionId::new(root.clone(), epoch))
            {
                return Err(AecInsertError::RecentlyConfirmed);
            }
            let mut minimum = self
                .epoch_source
                .as_ref()
                .and_then(|ledger| ledger.confirmation_epoch(&request.block.hash()));
            if let Some((winner, canonical)) = self.confirmed_epochs.get(&root) {
                if *winner != request.block.hash() {
                    return Err(AecInsertError::RecentlyConfirmed);
                }
                minimum = Some(minimum.map(|m| m.min(*canonical)).unwrap_or(*canonical));
            }
            if let Some(minimum) = minimum {
                return if epoch < minimum {
                    Ok(())
                } else {
                    Err(AecInsertError::RecentlyConfirmed)
                };
            }
        }
        if self.recently_confirmed.root_exists(&root) {
            return Err(AecInsertError::RecentlyConfirmed);
        }
        Ok(())
    }

    fn try_upgrade_priority_election(
        &mut self,
        request: &AecInsertRequest,
    ) -> Result<bool, AecInsertError> {
        let epoch = self.insertion_epoch.unwrap_or_else(|| self.current_epoch());
        let (upgraded, previous_behavior) =
            self.roots.try_upgrade_to_priority_election(request, epoch);

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
        let hash = request.block.hash();
        let mut election = Election::new(request.block, request.behavior, self.base_latency, now);
        election.epoch = self.insertion_epoch.unwrap_or_else(|| self.current_epoch());

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
        let mut results = Vec::new();
        for entry in self
            .roots
            .iter_mut()
            .filter(|e| e.election.qualified_root() == &fork.qualified_root())
        {
            results.push((
                entry.election.id(),
                entry.election.try_add_fork(fork, fork_tally),
            ));
        }
        let mut added = false;
        for (id, result) in results {
            match result {
                AddForkResult::Added | AddForkResult::Replaced(_) => {
                    if let AddForkResult::Replaced(removed) = result {
                        self.roots
                            .vote_router
                            .disconnect_epoch(&removed.hash(), id.epoch);
                        self.notify(AecFact::BlockDiscarded(removed.into()));
                    }
                    self.roots.vote_router.connect_epoch(fork.hash(), id);
                    self.notify(AecFact::BlockAddedToElection(fork.hash()));
                    self.stats.conflicts += 1;
                    added = true;
                }
                AddForkResult::TallyTooLow => self.notify(AecFact::BlockDiscarded(fork.clone())),
                _ => {}
            }
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

    pub fn election_for_id(&self, id: &rsnano_types::ElectionId) -> Option<&Election> {
        self.roots.get_id(id).map(|e| &e.election)
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

    pub fn remove_votes_in_epoch<'a>(
        &mut self,
        root: &QualifiedRoot,
        epoch: u64,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        if let Some(e) = self
            .roots
            .get_id_mut(&rsnano_types::ElectionId::new(root.clone(), epoch))
        {
            for voter in voters {
                e.election.remove_vote(voter);
            }
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
        if let Some(entry) = self.roots.erase_lowest(bucket_id) {
            self.cleanup_election(entry);
        }
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
            let mut confirmed_election =
                self.confirm_dependent_election(&confirmed_block, source_election, now);
            #[cfg(feature = "rai_protocol")]
            if let Some(epoch) = self
                .epoch_source
                .as_ref()
                .and_then(|l| l.confirmation_epoch(&confirmed_block.hash()))
            {
                confirmed_election.epoch = epoch;
            }

            for entry in self
                .roots
                .iter_mut()
                .filter(|e| e.election.qualified_root() == &confirmed_block.qualified_root())
            {
                if entry.election.epoch != confirmed_election.epoch
                    && entry.election.winner().hash() == confirmed_block.hash()
                {
                    continue;
                }
                if entry.election.winner().hash() == confirmed_block.hash() {
                    entry.election.force_confirm();
                } else {
                    entry.election.cancel();
                }
            }
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

        #[cfg(feature = "rai_protocol")]
        if self
            .epoch_source
            .as_ref()
            .and_then(|l| l.confirmation_epoch(&confirmed_block.hash()))
            .is_some_and(|canonical| corresponding.election.epoch != canonical)
        {
            let mut confirmed = ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::InactiveConfirmationHeight,
            );
            confirmed.epoch = self
                .epoch_source
                .as_ref()
                .unwrap()
                .confirmation_epoch(&confirmed_block.hash())
                .unwrap();
            return confirmed;
        }
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

    #[cfg(feature = "rai_protocol")]
    fn cache_confirmed_epoch(&mut self, hash: BlockHash, epoch: u64) {
        if self
            .confirmed_epoch_by_hash
            .get(&hash)
            .is_none_or(|e| epoch < *e)
        {
            self.confirmed_epoch_by_hash.insert(hash, epoch);
        }
    }

    fn block_confirmed(&mut self, block: SavedBlock, election: ConfirmedElection) {
        #[cfg(feature = "rai_protocol")]
        self.cache_confirmed_epoch(block.hash(), election.epoch);
        self.stats.block_confirmations[election.confirmation_type as usize] += 1;
        self.notify(AecFact::BlockConfirmed(block, election));
    }

    pub fn remove_recently_confirmed(&mut self, block_hash: &BlockHash) {
        self.recently_confirmed.erase(block_hash);
    }

    pub fn current_epoch(&self) -> u64 {
        self.epoch_source
            .as_ref()
            .map(|l| l.current_epoch())
            .unwrap_or(0)
    }

    pub fn insert_in_epoch(
        &mut self,
        request: AecInsertRequest,
        now: Timestamp,
        epoch: u64,
    ) -> Result<(), AecInsertError> {
        self.insertion_epoch = Some(epoch);
        let result = self.insert(request, now);
        self.insertion_epoch = None;
        result
    }

    pub fn apply_vote<'a>(
        &mut self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        #[cfg(feature = "rai_protocol")]
        if self.len() < self.max_elections {
            for hash in args.vote.filtered_blocks() {
                // Late same/later-epoch votes cannot open an earlier election.
                // Avoid ledger lookups under the AEC lock for this common case.
                if self
                    .confirmed_epoch_by_hash
                    .get(hash)
                    .is_some_and(|epoch| args.vote.epoch >= *epoch)
                {
                    continue;
                }
                if self.len() >= self.max_elections {
                    break;
                }
                if self
                    .roots
                    .election_for_epoch_mut(hash, args.vote.epoch)
                    .is_none()
                {
                    let block = self
                        .epoch_source
                        .as_ref()
                        .and_then(|l| {
                            use rsnano_ledger::AnySet;
                            l.any().get_block(hash)
                        })
                        .or_else(|| {
                            self.roots.election_for_block(hash).and_then(|e| {
                                match e.candidate_blocks().get(hash)? {
                                    rsnano_types::MaybeSavedBlock::Saved(block) => {
                                        Some(block.clone())
                                    }
                                    _ => None,
                                }
                            })
                        });
                    if let Some(block) = block {
                        let root = block.qualified_root();
                        let minimum = [
                            self.roots.election_for_root(&root).map(|e| e.epoch),
                            self.epoch_source
                                .as_ref()
                                .and_then(|l| l.confirmation_epoch(hash)),
                            self.confirmed_epochs.get(&root).map(|(_, epoch)| *epoch),
                        ]
                        .into_iter()
                        .flatten()
                        .min();
                        if minimum.is_some_and(|epoch| args.vote.epoch >= epoch)
                            || (minimum.is_none() && args.vote.epoch > self.current_epoch())
                        {
                            continue;
                        }
                        let _ = self.insert_in_epoch(
                            AecInsertRequest::new_manual(block, Default::default()),
                            args.now,
                            args.vote.epoch,
                        );
                    }
                }
            }
        }
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
            {
                self.cache_confirmed_epoch(entry.election.winner().hash(), entry.election.epoch);
                self.completed_ids
                    .insert(entry.election.id(), entry.election.winner().hash());
                let root = entry.election.qualified_root().clone();
                let epoch = entry.election.epoch;
                let minimum = self
                    .confirmed_epochs
                    .get(&root)
                    .map(|(_, e)| (*e).min(epoch))
                    .unwrap_or(epoch);
                self.confirmed_epochs
                    .insert(root, (entry.election.winner().hash(), minimum));
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
            block,
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        let now = Timestamp::new_test_instance();
        container.insert(request, now).unwrap();

        let rep_key = PrivateKey::from(1);
        #[cfg(not(feature = "rai_protocol"))]
        let received_vote = test_final_vote(&rep_key, block_hash);
        #[cfg(feature = "rai_protocol")]
        let received_vote = ReceivedVote::new(
            Arc::new(Vote::new_with_kind(
                &rep_key,
                vec![block_hash],
                0,
                rsnano_types::VoteKind::First,
            )),
            VoteDelivery::Direct,
            None,
        );

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

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use super::*;
    use crate::consensus::ReceivedVote;
    use rsnano_types::{BlockPriority, PrivateKey, Vote, VoteDelivery};
    #[test]
    fn rai_same_root_has_independent_elections_and_tallies() {
        let mut aec = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        for epoch in [0, 1] {
            aec.insert_in_epoch(
                AecInsertRequest::new_manual(block.clone(), BlockPriority::default()),
                now,
                epoch,
            )
            .unwrap();
        }
        assert_eq!(aec.len(), 2);
        let key = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(key.public_key(), Amount::MAX);
        let quorum = QuorumSnapshot::new_test_instance();
        let vote: FilteredVote = ReceivedVote::new(
            std::sync::Arc::new(Vote::new_in_epoch(
                &key,
                Vote::TIMESTAMP_MIN,
                0,
                vec![block.hash()],
                1,
            )),
            VoteDelivery::Direct,
            None,
        )
        .into();
        assert_eq!(
            aec.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: &weights,
                quorum_snapshot: &quorum,
                now
            })
            .get(&block.hash()),
            Some(&Ok(()))
        );
        assert_eq!(aec.len(), 1);
        let other = aec.election_for_block(&block.hash()).unwrap();
        assert_eq!(other.epoch, 0);
        assert_eq!(other.vote_count(), 0);
        assert!(!other.is_confirmed());
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod earlier_vote_tests {
    use super::*;
    use crate::consensus::ReceivedVote;
    use rsnano_types::{PrivateKey, Vote, VoteDelivery};
    #[test]
    fn rai_only_earlier_votes_create_another_slot_election() {
        let mut aec = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        aec.insert_in_epoch(
            AecInsertRequest::new_manual(block.clone(), Default::default()),
            now,
            2,
        )
        .unwrap();
        let key = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(key.public_key(), Amount::raw(1));
        let quorum = QuorumSnapshot::new_test_instance();
        for (epoch, expected_len) in [(3, 1), (1, 2), (0, 3)] {
            let vote: FilteredVote = ReceivedVote::new(
                std::sync::Arc::new(Vote::new_in_epoch(
                    &key,
                    Vote::TIMESTAMP_MAX,
                    Vote::DURATION_MAX,
                    vec![block.hash()],
                    epoch,
                )),
                VoteDelivery::Direct,
                None,
            )
            .into();
            aec.apply_vote(ApplyVoteArgs {
                vote: &vote,
                rep_weights: &weights,
                quorum_snapshot: &quorum,
                now,
            });
            assert_eq!(aec.len(), expected_len);
        }
        let original = aec
            .election_for_id(&rsnano_types::ElectionId::new(block.qualified_root(), 2))
            .unwrap();
        assert_eq!(original.vote_count(), 0);
        let earlier = aec
            .election_for_id(&rsnano_types::ElectionId::new(block.qualified_root(), 1))
            .unwrap();
        assert_eq!(earlier.vote_count(), 1);
        assert!(!earlier.is_confirmed());
    }
}
