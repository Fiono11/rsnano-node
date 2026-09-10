use std::{cmp::max, collections::HashMap, time::Duration};

use strum::EnumCount;

#[cfg(feature = "rai_protocol")]
use rsnano_ledger::AnySet;
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
    #[cfg(feature = "rai_protocol")]
    notarization_notifications: super::notarization_notifications::NotarizationNotifications,
    #[cfg(feature = "rai_protocol")]
    pub block_tree: crate::consensus::RaiBlockTree,
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
    report_outcomes: bool,
}

impl ActiveElectionsContainer {
    pub fn termination_audit(&self, offset: usize) -> serde_json::Value {
        let mut page = self.stats.vote_counter.audit.page(offset);
        #[cfg(feature = "rai_protocol")]
        if offset == 0 && self.stats.vote_counter.audit.enabled() {
            let diagnostic_limit = std::env::var("NANOSPAM_DIAGNOSTIC_LIMIT")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(256)
                .min(10_000);
            page["active"] = serde_json::json!(
                self.roots
                    .round_robin()
                    .filter(|entry| !entry.election.is_confirmed())
                    .take(diagnostic_limit)
                    .map(|entry| {
                        let e = &entry.election;
                        let mut value = e.termination_diagnostic();
                        if let Some(ledger) = &self.epoch_source {
                            let any = ledger.any();
                            value["dependencies_confirmed"] = serde_json::json!(
                                e.candidate_blocks()
                                    .iter()
                                    .map(|(hash, block)| (
                                        *hash,
                                        any.dependencies_confirmed_for_unsaved_block(
                                            &block.clone().into()
                                        )
                                    ))
                                    .collect::<Vec<_>>()
                            );
                        }
                        value
                    })
                    .collect::<Vec<_>>()
            );
            page["diagnostic_limit"] = serde_json::json!(diagnostic_limit);
            page["scheduling_len"] = serde_json::json!(self.roots.scheduling_len());
        }
        page
    }

    #[cfg(feature = "rai_protocol")]
    pub fn take_notarization_notifications(
        &mut self,
    ) -> Vec<(rsnano_types::ElectionId, BlockHash)> {
        self.notarization_notifications.take()
    }

    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            #[cfg(feature = "rai_protocol")]
            notarization_notifications: Default::default(),
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
            #[cfg(feature = "rai_protocol")]
            block_tree: Default::default(),
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
            report_outcomes: std::env::var_os("NANOSPAM_ELECTION_METRICS").is_some(),
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
            if self.epoch_source.as_ref().is_some_and(|ledger| {
                epoch
                    < ledger
                        .closed_epoch_count
                        .load(std::sync::atomic::Ordering::Acquire)
                    && ledger
                        .canonical_confirmation_epoch(&request.block.hash())
                        .is_none_or(|e| e > epoch)
            }) {
                return Err(AecInsertError::RecentlyConfirmed);
            }

            if self
                .completed_ids
                .contains_key(&rsnano_types::ElectionId::new(root.clone(), epoch))
            {
                return Err(AecInsertError::RecentlyConfirmed);
            }
            let mut minimum = self
                .epoch_source
                .as_ref()
                .and_then(|ledger| ledger.canonical_confirmation_epoch(&request.block.hash()));
            if let Some((winner, canonical)) = self.confirmed_epochs.get(&root) {
                if *winner != request.block.hash() {
                    return Err(AecInsertError::RecentlyConfirmed);
                }
                minimum = Some(minimum.map(|m| m.min(*canonical)).unwrap_or(*canonical));
            }
            if let Some(minimum) = minimum {
                return if epoch <= minimum {
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

        self.stats
            .vote_counter
            .audit
            .record(0, root.clone(), hash, election.epoch);
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
        for id in self.roots.ids_for_root(&fork.qualified_root()) {
            let entry = self.roots.get_id_mut(&id).unwrap();
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
        let current_size = self.roots.scheduling_len() as i64;
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

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn pending_epoch_drain(
        &self,
        epoch: u64,
        local_first: &[rsnano_types::ElectionId],
    ) -> Vec<rsnano_types::ElectionId> {
        let mut required: std::collections::HashSet<_> = local_first.iter().cloned().collect();
        required.extend(self.roots.iter().filter_map(|entry| {
            let e = &entry.election;
            (e.epoch == epoch && e.has_f_plus_one_first_votes()).then(|| e.id())
        }));
        required
            .into_iter()
            .filter(|id| {
                !self
                    .election_for_id(id)
                    .is_some_and(|e| e.has_quorum() || e.is_confirmed() || e.is_timed_out())
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn assert_epoch_close(&self, epoch: u64, hashes: &[BlockHash]) {
        for entry in self
            .block_tree
            .entries()
            .filter(|e| e.epoch == epoch && e.finalized)
        {
            if hashes.binary_search(&entry.hash()).is_err() {
                crate::consensus::epoch_closer::debug_trace(
                    || serde_json::json!({"type":"omitted_finalized","epoch":epoch,"hash":entry.hash(),"root":entry.root,"election":self.election_for_id(&rsnano_types::ElectionId::new(entry.root.clone(), entry.epoch)).map(|e| e.termination_diagnostic())}),
                );
            }
            assert!(
                hashes.binary_search(&entry.hash()).is_ok(),
                "epoch close omitted finalized block: epoch={} root={:?} hash={}",
                epoch,
                entry.root,
                entry.hash()
            );
        }
        for entry in self.roots.iter().filter(|e| e.election.epoch == epoch) {
            let election = &entry.election;
            assert!(
                !election.is_confirmed() || hashes.binary_search(&election.winner().hash()).is_ok(),
                "epoch close omitted finalized election: epoch={} root={:?} hash={}",
                epoch,
                election.qualified_root(),
                election.winner().hash()
            );
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn discard_closed_epoch(&mut self, epoch: u64, hashes: &[BlockHash]) -> usize {
        self.assert_epoch_close(epoch, hashes);
        let removed = self.roots.drain_filter(|entry| {
            entry.election.epoch == epoch
                && !entry
                    .election
                    .candidate_blocks()
                    .keys()
                    .any(|hash| hashes.binary_search(hash).is_ok())
        });
        let count = removed.len();
        for entry in removed {
            self.cleanup_election(entry);
        }
        count
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
                let bucket_vacancy = if self.roots.scheduling_len() >= self.max_elections {
                    0
                } else {
                    self.max_elections_per_bucket as isize - bucket.election_count as isize
                };

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
        #[cfg(feature = "rai_protocol")]
        return;
        #[cfg(not(feature = "rai_protocol"))]
        {
            let removed = self.roots.drain_filter(|i| i.election.state().has_ended());

            for entry in removed {
                self.cleanup_election(entry);
            }
        }
    }

    pub fn erase(&mut self, root: &QualifiedRoot) -> bool {
        #[cfg(feature = "rai_protocol")]
        {
            let _ = root;
            return false;
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            let Some(entry) = self.roots.erase(root) else {
                return false;
            };
            self.cleanup_election(entry);
            true
        }
    }

    pub fn erase_lowest_prio_election(&mut self, bucket_id: usize) {
        #[cfg(feature = "rai_protocol")]
        {
            let _ = bucket_id;
            return;
        }
        #[cfg(not(feature = "rai_protocol"))]
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
            let implicit = source_election
                .as_ref()
                .is_none_or(|e| e.winner.hash() != confirmed_block.hash());
            let confirmed_election =
                self.confirm_dependent_election(&confirmed_block, source_election, now);
            #[cfg(not(feature = "rai_protocol"))]
            for entry in self
                .roots
                .iter_mut()
                .filter(|e| e.election.qualified_root() == &confirmed_block.qualified_root())
            {
                if entry.election.winner().hash() == confirmed_block.hash() {
                    entry.election.force_confirm();
                } else {
                    entry.election.cancel();
                }
            }
            self.stats.vote_counter.audit.record(
                if implicit { 4 } else { 2 },
                confirmed_block.qualified_root(),
                confirmed_block.hash(),
                confirmed_election.epoch,
            );
            // Ledger application is not a certificate for this root/voting epoch.
            // Certificate outcomes are inserted exclusively by apply_vote.
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

        #[cfg(feature = "rai_protocol")]
        {
            return ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::InactiveConfirmationHeight,
            );
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
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

    /// Repair using ordinary publish/confirm_ack messages. Signed votes retain their epoch.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn recovery_entries(
        &self,
        requests: &[(BlockHash, rsnano_types::Root)],
        epoch: u64,
    ) -> Vec<rsnano_types::RaiBlockTreeEntry> {
        let mut result = std::collections::BTreeMap::new();
        for (hash, root) in requests {
            let qualified = self
                .roots
                .election_for_block(hash)
                .filter(|e| e.winner().root() == *root)
                .map(|e| e.qualified_root().clone())
                .unwrap_or_else(|| QualifiedRoot::new(*root, (*root).into()));
            let mut entries = self.block_tree.for_root(&qualified);
            if entries.is_empty() {
                entries = self
                    .block_tree
                    .for_root(&QualifiedRoot::new(*root, BlockHash::ZERO));
            }
            let canonical = entries
                .iter()
                .map(|e| {
                    (
                        if e.finalized {
                            0
                        } else if e.block.is_some() {
                            1
                        } else {
                            2
                        },
                        e.epoch,
                    )
                })
                .min()
                .map(|(_, e)| e);
            for entry in entries {
                if entry.epoch == epoch || Some(entry.epoch) == canonical {
                    result.insert((entry.root.clone(), entry.epoch, entry.hash()), entry);
                }
            }
        }
        result.into_values().collect()
    }

    pub fn apply_vote<'a>(
        &mut self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        #[cfg(feature = "rai_protocol")]
        {
            for hash in args.vote.filtered_blocks() {
                // Late same/later-epoch votes cannot open an earlier election.
                // Avoid ledger lookups under the AEC lock for this common case.
                if self
                    .confirmed_epoch_by_hash
                    .get(hash)
                    .is_some_and(|epoch| args.vote.epoch > *epoch)
                {
                    continue;
                }
                if self
                    .roots
                    .election_for_epoch_mut(hash, args.vote.epoch)
                    .is_none()
                {
                    if let Some(candidate) = self
                        .roots
                        .election_for_block(hash)
                        .and_then(|e| e.candidate_blocks().get(hash))
                        .cloned()
                    {
                        self.try_add_fork(&candidate.into(), Amount::ZERO);
                    }
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
                            self.epoch_source
                                .as_ref()
                                .and_then(|l| l.canonical_confirmation_epoch(hash)),
                            self.confirmed_epochs.get(&root).map(|(_, epoch)| *epoch),
                        ]
                        .into_iter()
                        .flatten()
                        .min();
                        if minimum.is_some_and(|epoch| args.vote.epoch > epoch)
                            || (minimum.is_none()
                                && self.roots.election_for_root(&root).is_none()
                                && args.vote.epoch > self.current_epoch())
                        {
                            continue;
                        }
                        self.try_add_fork(&block.clone().into(), Amount::ZERO);
                        // Capacity limits new elections, never recovery of a retained one.
                        if self.roots.scheduling_len() >= self.max_elections
                            && self.roots.election_for_root(&root).is_none()
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
        #[cfg(feature = "rai_protocol")]
        for entry in result.tree_entries {
            let epoch = entry.epoch;
            let hash = entry.hash();
            let finalized = entry.finalized;
            let root = entry.root.clone();
            let timeout = entry.block.is_none();
            if let (Some(ledger), Some(block)) = (&self.epoch_source, &entry.block) {
                ledger.record_epoch_block(epoch, block.clone());
            }
            let changed = self
                .block_tree
                .insert(entry)
                .expect("Conflicting locally established outcomes");
            if changed && self.report_outcomes {
                let first_vote_us = self
                    .election_for_id(&rsnano_types::ElectionId::new(root.clone(), epoch))
                    .and_then(|e| e.first_vote_observed)
                    .map(|first| first.elapsed(args.now).as_micros() as u64);
                self.notify(AecFact::ElectionOutcome(
                    root,
                    hash,
                    epoch,
                    finalized,
                    timeout,
                    first_vote_us,
                ));
            }
            if epoch == 1 && changed {
                crate::consensus::epoch_closer::debug_trace(
                    || serde_json::json!({"type":"block_certificate","epoch":epoch,"hash":hash,"finalized":finalized,"trigger_voter":args.vote.voter,"trigger_kind":format!("{:?}",args.vote.kind)}),
                );
            }
        }
        #[cfg(feature = "rai_protocol")]
        for item in result.notarization_ready {
            self.notarization_notifications.push(item);
        }
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
        #[cfg(feature = "rai_protocol")]
        crate::consensus::epoch_closer::debug_trace(
            || serde_json::json!({"type":"vote_applied","epoch":args.vote.epoch,"voter":args.vote.voter,"kind":format!("{:?}",args.vote.kind),"results":result.per_block.iter().map(|(h,r)| (h,format!("{:?}",r))).collect::<Vec<_>>()}),
        );
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

        #[cfg(not(feature = "rai_protocol"))]
        assert!(container.election_for_block(&block_hash).is_none());
        #[cfg(feature = "rai_protocol")]
        assert!(
            container
                .election_for_block(&block_hash)
                .unwrap()
                .is_confirmed()
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
    fn epoch_drain_waits_for_f_plus_one_despite_local_first_timeout() {
        use rsnano_types::{ElectionId, VoteKind};
        let mut aec = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        aec.insert_in_epoch(
            AecInsertRequest::new_manual(block.clone(), Default::default()),
            now,
            1,
        )
        .unwrap();
        let id = ElectionId::new(block.qualified_root(), 1);
        let weights = (1..=6)
            .map(|i| (PrivateKey::from(i).public_key(), Amount::raw(100)))
            .collect();
        let add = |aec: &mut ActiveElectionsContainer, rep, kind| {
            let e = &mut aec.roots.get_id_mut(&id).unwrap().election;
            e.add_kudzu_vote(
                std::sync::Arc::new(Vote::new_with_kind(
                    &PrivateKey::from(rep),
                    vec![block.hash()],
                    1,
                    kind,
                )),
                block.hash(),
                now,
            )
            .unwrap();
            e.update_kudzu_tallies(&weights, Amount::raw(600));
        };
        add(&mut aec, 1, VoteKind::FirstTimeout);
        add(&mut aec, 2, VoteKind::First);
        assert!(aec.pending_epoch_drain(1, &[]).is_empty());
        assert_eq!(aec.pending_epoch_drain(1, &[id.clone()]), vec![id.clone()]);
        add(&mut aec, 3, VoteKind::First);
        assert_eq!(aec.pending_epoch_drain(1, &[]), vec![id.clone()]);
        assert!(aec.pending_epoch_drain(0, &[]).is_empty());
        add(&mut aec, 4, VoteKind::FirstTimeout);
        add(&mut aec, 5, VoteKind::FirstTimeout);
        add(&mut aec, 6, VoteKind::FirstTimeout);
        assert!(aec.pending_epoch_drain(1, &[]).is_empty());
    }

    #[test]
    fn epoch_close_discards_only_omitted_epoch_elections() {
        let mut aec = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        for epoch in [0, 1] {
            aec.insert_in_epoch(
                AecInsertRequest::new_manual(block.clone(), Default::default()),
                now,
                epoch,
            )
            .unwrap();
        }
        assert_eq!(aec.discard_closed_epoch(0, &[]), 1);
        assert_eq!(aec.count_by_behavior(ElectionBehavior::Manual), 1);
        assert!(
            aec.election_for_id(&rsnano_types::ElectionId::new(block.qualified_root(), 0))
                .is_none()
        );
        assert!(
            aec.election_for_id(&rsnano_types::ElectionId::new(block.qualified_root(), 1))
                .is_some()
        );
        assert_eq!(aec.discard_closed_epoch(1, &[block.hash()]), 0);
        assert_eq!(aec.count_by_behavior(ElectionBehavior::Manual), 1);
        assert_eq!(aec.discard_closed_epoch(1, &[]), 1);
        assert_eq!(aec.count_by_behavior(ElectionBehavior::Manual), 0);
    }

    #[test]
    #[should_panic(expected = "epoch close omitted finalized block")]
    fn epoch_close_asserts_before_discarding_finalized_block() {
        let mut aec = ActiveElectionsContainer::default();
        let mut entry =
            rsnano_types::RaiBlockTreeEntry::notarized(SavedBlock::new_test_instance().into(), 0);
        entry.finalized = true;
        aec.block_tree.insert(entry).unwrap();
        aec.discard_closed_epoch(0, &[]);
    }

    #[test]
    fn old_epoch_vote_recovers_candidate_known_only_in_later_epoch() {
        use rsnano_types::{ElectionId, StateBlockArgs, VoteKind};
        let mut aec = ActiveElectionsContainer::default();
        let args = StateBlockArgs::new_test_instance();
        let block = SavedBlock::new_test_instance_with(args.clone().into());
        let fork: Block = StateBlockArgs {
            representative: 999.into(),
            ..args
        }
        .into();
        let now = Timestamp::new_test_instance();
        for epoch in [0, 1] {
            aec.insert_in_epoch(
                AecInsertRequest::new_manual(block.clone(), Default::default()),
                now,
                epoch,
            )
            .unwrap();
        }
        let id = ElectionId::new(block.qualified_root(), 1);
        aec.roots
            .get_id_mut(&id)
            .unwrap()
            .election
            .try_add_fork(&fork, Amount::ZERO);
        aec.roots.vote_router.connect_epoch(fork.hash(), id);
        aec.max_elections = aec.roots.scheduling_len();
        let key = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(key.public_key(), Amount::raw(1));
        let vote: FilteredVote = ReceivedVote::new(
            std::sync::Arc::new(Vote::new_with_kind(
                &key,
                vec![fork.hash()],
                0,
                VoteKind::Notarize,
            )),
            VoteDelivery::Direct,
            None,
        )
        .into();
        let result = aec.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(result[&fork.hash()], Ok(()));
        assert!(
            aec.election_for_id(&ElectionId::new(block.qualified_root(), 0))
                .unwrap()
                .candidate_blocks()
                .contains_key(&fork.hash())
        );
    }

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
        assert_eq!(aec.len(), 2);
        assert!(
            aec.election_for_id(&rsnano_types::ElectionId::new(block.qualified_root(), 1))
                .unwrap()
                .is_confirmed()
        );
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
    fn rai_other_epochs_recover_independently_for_known_election() {
        let mut aec = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        aec.insert_in_epoch(
            AecInsertRequest::new_manual(block.clone(), Default::default()),
            now,
            2,
        )
        .unwrap();
        aec.max_elections = aec.roots.scheduling_len();
        let key = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(key.public_key(), Amount::raw(1));
        let quorum = QuorumSnapshot::new_test_instance();
        for (epoch, expected_len) in [(3, 2), (1, 3), (0, 4)] {
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

#[cfg(all(test, feature = "rai_protocol"))]
mod notarized_admission_tests {
    use super::*;
    use crate::consensus::ReceivedVote;
    use rsnano_types::{PrivateKey, StateBlockArgs, Vote, VoteDelivery, VoteKind};

    fn apply(aec: &mut ActiveElectionsContainer, rep: u64, hash: BlockHash, kind: VoteKind) {
        let mut weights = RepWeights::default();
        for rep in 1..=6 {
            weights.put(PrivateKey::from(rep).public_key(), Amount::raw(100));
        }
        let mut quorum = QuorumSnapshot::new_test_instance();
        quorum.online_weight = Amount::raw(600);
        quorum.trended_or_min_weight = Amount::raw(600);
        let vote: FilteredVote = ReceivedVote::new(
            std::sync::Arc::new(Vote::new_with_kind(
                &PrivateKey::from(rep),
                vec![hash],
                0,
                kind,
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
                now: Timestamp::new_test_instance()
            })[&hash],
            Ok(())
        );
    }

    #[test]
    fn drain_and_snapshot_observe_the_same_notarized_block_before_finalization() {
        use rsnano_ledger::{
            LedgerBuilder, LedgerConstants, test_helpers::UnsavedBlockLatticeBuilder,
        };
        let path = std::env::temp_dir().join(format!("rai-drain-snapshot-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let ledger = std::sync::Arc::new(
                LedgerBuilder::new(path.join("data.ldb"))
                    .constants(LedgerConstants::dev())
                    .init_thread_count(1)
                    .finish()
                    .unwrap(),
            );
            ledger.configure_epoch_length(40).unwrap();
            let mut lattice = UnsavedBlockLatticeBuilder::new();
            let block = lattice.genesis().send(100, 1);
            let saved = ledger.process_one(&block).unwrap();
            let mut aec = ActiveElectionsContainer::default();
            aec.epoch_source = Some(ledger.clone());
            aec.insert(
                AecInsertRequest::new_priority(saved, Default::default()),
                Timestamp::new_test_instance(),
            )
            .unwrap();
            let id = rsnano_types::ElectionId::new(block.qualified_root(), 0);
            for rep in 1..=3 {
                apply(&mut aec, rep, block.hash(), VoteKind::First);
            }
            assert!(!aec.pending_epoch_drain(0, &[id.clone()]).is_empty());
            assert!(!ledger.epoch_close_candidate(0).contains(&block.hash()));
            apply(&mut aec, 4, block.hash(), VoteKind::First);
            assert!(aec.pending_epoch_drain(0, &[id]).is_empty());
            assert!(
                !aec.election_for_block(&block.hash())
                    .unwrap()
                    .is_confirmed()
            );
            let snapshot = ledger.epoch_close_candidate(0);
            assert!(
                snapshot.contains(&block.hash()),
                "drained notarized block must be in the snapshot before cementation"
            );
            assert!(ledger.epoch_close_candidate_valid(0, &snapshot));
            for rep in 1..=4 {
                apply(&mut aec, rep, block.hash(), VoteKind::Final);
            }
            aec.assert_epoch_close(0, &snapshot);
            assert_eq!(ledger.epoch_close_candidate(0), snapshot);
        }
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn epoch_cementation_cannot_manufacture_a_final_certificate_over_a_timeout() {
        let mut aec = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        aec.insert(
            AecInsertRequest::new_priority(block.clone(), Default::default()),
            now,
        )
        .unwrap();
        for rep in 1..=4 {
            apply(&mut aec, rep, block.hash(), VoteKind::FirstTimeout);
        }
        aec.confirm_dependent_elections(vec![(block.clone(), None)], now);
        let entries = aec.block_tree.for_root(&block.qualified_root());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].block.is_none());
        assert!(
            !aec.election_for_block(&block.hash())
                .unwrap()
                .is_confirmed()
        );
    }

    #[test]
    fn timeout_certificate_releases_capacity_and_recovers_without_confirming_block() {
        let mut aec = ActiveElectionsContainer::default();
        let capacity = aec.vacancy();
        let block = SavedBlock::new_test_instance();
        aec.insert(
            AecInsertRequest::new_priority(block.clone(), Default::default()),
            Timestamp::new_test_instance(),
        )
        .unwrap();
        apply(&mut aec, 1, block.hash(), VoteKind::First);
        apply(&mut aec, 1, block.hash(), VoteKind::Timeout);
        for rep in 2..=4 {
            apply(&mut aec, rep, block.hash(), VoteKind::FirstTimeout);
        }
        let e = aec.election_for_block(&block.hash()).unwrap();
        assert!(e.is_timed_out());
        assert!(!e.is_confirmed());
        assert_eq!(aec.vacancy(), capacity);
        let entries = aec.recovery_entries(&[(block.hash(), block.root())], 1);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].epoch, 0);
        assert!(entries[0].block.is_none());
        assert!(!entries[0].finalized);
    }

    #[test]
    fn unfinished_election_survives_expiration_and_eviction() {
        let mut aec = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        aec.insert(
            AecInsertRequest::new_priority(block.clone(), Default::default()),
            now,
        )
        .unwrap();
        for rep in 1..=3 {
            apply(&mut aec, rep, block.hash(), VoteKind::First);
        }
        aec.transition_time(now + Duration::from_secs(600));
        aec.erase_lowest_prio_election(aec.find_bucket(&block.qualified_root()).unwrap());
        assert!(!aec.erase(&block.qualified_root()));
        assert_eq!(aec.len(), 1);
        assert!(!aec.election_for_block(&block.hash()).unwrap().has_quorum());
        apply(&mut aec, 4, block.hash(), VoteKind::First);
        assert!(aec.election_for_block(&block.hash()).unwrap().has_quorum());
    }

    #[test]
    fn notarized_root_releases_capacity_for_existing_and_later_epochs() {
        let mut aec = ActiveElectionsContainer::default();
        let capacity = aec.vacancy();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        for epoch in [0, 1] {
            aec.insert_in_epoch(
                AecInsertRequest::new_manual(block.clone(), Default::default()),
                now,
                epoch,
            )
            .unwrap();
        }
        assert_eq!(aec.vacancy(), capacity - 1);
        for rep in 1..=4 {
            apply(&mut aec, rep, block.hash(), VoteKind::First);
        }
        assert_eq!(aec.vacancy(), capacity);
        aec.insert_in_epoch(
            AecInsertRequest::new_manual(block.clone(), Default::default()),
            now,
            2,
        )
        .unwrap();
        assert_eq!(aec.vacancy(), capacity);
        assert_eq!(aec.iter_round_robin().count(), 3);
        let later = aec
            .election_for_id(&rsnano_types::ElectionId::new(block.qualified_root(), 1))
            .unwrap();
        assert!(
            !later.has_quorum(),
            "Admission accounting is not a certificate"
        );
        assert!(!later.is_confirmed());
    }

    #[test]
    fn notarized_election_releases_slot_and_can_still_finalize() {
        let mut aec = ActiveElectionsContainer::default();
        let capacity = aec.vacancy();
        let block = SavedBlock::new_test_instance();
        aec.insert(
            AecInsertRequest::new_priority(block.clone(), Default::default()),
            Timestamp::new_test_instance(),
        )
        .unwrap();
        assert_eq!(aec.vacancy(), capacity - 1);
        for rep in 1..=4 {
            apply(&mut aec, rep, block.hash(), VoteKind::First);
        }
        assert_eq!(aec.vacancy(), capacity);
        assert_eq!(aec.len(), 1);
        assert_eq!(
            aec.bucket_len(aec.find_bucket(&block.qualified_root()).unwrap()),
            0
        );
        assert_eq!(aec.iter_round_robin().count(), 1);
        for rep in 1..=4 {
            apply(&mut aec, rep, block.hash(), VoteKind::Final);
        }
        assert!(
            aec.election_for_block(&block.hash())
                .unwrap()
                .is_confirmed()
        );
        assert_eq!(aec.vacancy(), capacity);
    }

    #[test]
    fn notarized_election_keeps_collecting_other_certificates_without_consuming_slot() {
        let mut aec = ActiveElectionsContainer::default();
        let capacity = aec.vacancy();
        let args = StateBlockArgs::new_test_instance();
        let block = SavedBlock::new_test_instance_with(args.clone().into());
        let fork: Block = StateBlockArgs {
            representative: 999.into(),
            ..args
        }
        .into();
        aec.insert(
            AecInsertRequest::new_priority(block.clone(), Default::default()),
            Timestamp::new_test_instance(),
        )
        .unwrap();
        assert!(aec.try_add_fork(&fork, Amount::ZERO));
        for rep in 1..=6 {
            apply(
                &mut aec,
                rep,
                if rep <= 3 { block.hash() } else { fork.hash() },
                VoteKind::First,
            );
        }
        apply(&mut aec, 4, block.hash(), VoteKind::Notarize);
        assert_eq!(aec.vacancy(), capacity);
        apply(&mut aec, 1, fork.hash(), VoteKind::Notarize);
        assert_eq!(aec.vacancy(), capacity);
        let election = aec.election_for_block(&block.hash()).unwrap();
        assert!(election.has_kudzu_certificate(block.hash(), VoteKind::Notarize));
        assert!(election.has_kudzu_certificate(fork.hash(), VoteKind::Notarize));
        assert!(!election.is_confirmed());
        let entries = aec.recovery_entries(&[(block.hash(), block.root())], 1);
        assert_eq!(entries.len(), 2, "Recover both canonical notarized forks");
        assert!(entries.iter().any(|entry| entry.hash() == fork.hash()));
        assert!(
            entries
                .iter()
                .all(|entry| entry.epoch == 0 && !entry.finalized)
        );
        aec.transition_time(Timestamp::new_test_instance() + Duration::from_secs(600));
        assert_eq!(aec.len(), 1);
    }
}
