use std::{
    cmp::max,
    collections::{BTreeMap, HashMap},
    mem::size_of,
    time::Duration,
};

use strum::EnumCount;

use rsnano_ledger::RepWeights;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Account, Amount, Block, BlockHash, BlockPriority, ConsensusEpoch, PublicKey, QualifiedRoot,
    SavedBlock, TimePriority, VoteError, VoteKind,
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
            AddForkResult, CertificateEvidence, ConfirmationType, ConfirmedElection, Election,
            ElectionBehavior, ElectionId, ElectionState, EpochSlot, FinalStateHash,
            KudzuThresholds, LocalSlotState, VoteType,
        },
        election_schedulers::priority::bucket_count,
        filtered_vote::FilteredVote,
        vote_generation::VoteTarget,
    },
    representatives::QuorumSnapshot,
};

use super::{
    ActiveElectionsConfig, ActiveElectionsInfo, AecFact, AecInsertError, AecInsertRequest, Entry,
    RootContainer,
    apply_vote_helper::ApplyVoteHelper,
    cooldown_controller::{AecCooldownReason, CooldownController, CooldownResult},
    epoch_states::{EpochStates, FinalizedInstance},
    recently_confirmed_cache::RecentlyConfirmedCache,
    slot_states::SlotStates,
    stats::AecStats,
};

/// Kudzu never evicts, so a full bucket makes the scheduler hold its blocks
/// until an election in it terminates. The spam workload keeps its accounts
/// in a few buckets, and around an epoch switch the in-flight blocks have two
/// instances each: the per-bucket cap would stall the scheduler for seconds.
/// The AEC size bounds the elections as a whole.
pub(crate) fn per_bucket_cap(max_elections: usize) -> usize {
    if cfg!(feature = "rai_protocol") {
        max_elections
    } else {
        max(max_elections / bucket_count(), 1)
    }
}

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
    /// Kudzu: what this node voted per slot (account height) and epoch. Shared
    /// by all elections at that height and dropped once the height is finalized.
    slots: SlotStates,
    /// Kudzu: exit final votes of elections which were finalized and erased
    /// before the voter could pick them up (line 11 still applies)
    pending_kudzu_votes: Vec<VoteTarget>,
    /// RAI: the epoch new elections are started in
    current_epoch: ConsensusEpoch,
    /// RAI: elections of the current epoch which got a certificate so far
    decided_in_current_epoch: usize,
    /// RAI: `decided_in_current_epoch` at which the epoch advances; 0 never
    epoch_terminated_elections: usize,
    /// RAI: what each epoch finalized explicitly
    epoch_states: EpochStates,
    /// RAI: the newest epoch each representative was seen voting in
    rep_epochs: HashMap<PublicKey, ConsensusEpoch>,
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
            max_elections_per_bucket: per_bucket_cap(config.max_elections),
            stats: Default::default(),
            slots: SlotStates::default(),
            pending_kudzu_votes: Vec::new(),
            current_epoch: ConsensusEpoch::ZERO,
            decided_in_current_epoch: 0,
            epoch_terminated_elections: config.epoch_terminated_elections,
            epoch_states: EpochStates::default(),
            rep_epochs: HashMap::new(),
        }
    }

    /// RAI: a vote of an epoch ahead of this node. Once more than f of the
    /// weight votes in a later epoch at least one correct representative has
    /// reached the epoch's end, and this node follows at once instead of
    /// finishing its own count: the blocks in flight during the switch get
    /// their decision as soon as every replica is in the same epoch.
    fn observe_epoch(&mut self, args: &ApplyVoteArgs) {
        let vote = &args.vote;
        if self.epoch_terminated_elections == 0 || vote.epoch <= self.current_epoch {
            return;
        }
        self.rep_epochs.insert(vote.voter, vote.epoch);
        let next = self.current_epoch.next();
        let ahead: Amount = self
            .rep_epochs
            .iter()
            .filter(|(_, epoch)| **epoch >= next)
            .map(|(rep, _)| args.rep_weights.weight(rep))
            .sum();
        let thresholds = KudzuThresholds::from_quorum(args.quorum_snapshot);
        if ahead > thresholds.f {
            self.stats.epochs_followed += 1;
            self.advance_epoch(args.now);
        }
    }

    /// RAI: elections of the current epoch which got a certificate so far
    pub fn decided_in_current_epoch(&self) -> usize {
        self.decided_in_current_epoch
    }

    /// RAI: whether this block was finalized explicitly in the given epoch
    pub fn finalized_in_epoch(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> bool {
        self.epoch_states.finalized_in_epoch(hash, epoch)
    }

    /// RAI: whether this block was finalized explicitly in any epoch
    pub fn is_finalized(&self, hash: &BlockHash) -> bool {
        self.epoch_states.is_finalized(hash)
    }

    /// RAI: whether this node cast its final vote for the block in the epoch
    pub fn final_voted_in_epoch(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> bool {
        self.epoch_states.final_voted_in_epoch(hash, epoch)
    }

    /// RAI: the explicitly finalized state per epoch
    pub fn finalized_by_epoch(&self) -> &BTreeMap<ConsensusEpoch, FinalStateHash> {
        self.epoch_states.by_epoch()
    }

    /// RAI: the blocks finalized explicitly in the given epoch
    pub fn finalized_in(&self, epoch: ConsensusEpoch) -> Vec<(Account, u64, BlockHash)> {
        self.epoch_states.finalized_in(epoch)
    }

    /// RAI: count the elections of the current epoch which just got their
    /// first certificate and advance the epoch once enough have
    fn count_decided(&mut self, decided: &[ConsensusEpoch], now: Timestamp) {
        self.decided_in_current_epoch += decided
            .iter()
            .filter(|epoch| **epoch == self.current_epoch)
            .count();
        if self.epoch_terminated_elections > 0
            && self.decided_in_current_epoch >= self.epoch_terminated_elections
        {
            self.advance_epoch(now);
        }
    }

    /// RAI: leave the current epoch and start the next one. The instances of
    /// the old epoch run on to their termination: this node proposed in
    /// them, so it keeps voting in them. Every replica which started one of
    /// them before its own switch does the same; a replica which learns of a
    /// block only after switching proposes it in the new epoch, and its votes
    /// open that instance here.
    fn advance_epoch(&mut self, now: Timestamp) {
        let _ = now;
        self.current_epoch = self.current_epoch.next();
        self.decided_in_current_epoch = 0;
        self.stats.epochs_advanced += 1;
        self.notify(AecFact::EpochAdvanced(self.current_epoch));
        if cfg!(feature = "rai_protocol") {
            let unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default();
            eprintln!(
                "EPOCH_ADVANCED t={} epoch={} elections={} active={}",
                unix_ms,
                self.current_epoch,
                self.roots.len(),
                self.roots.active_len()
            );
        }
    }

    /// RAI: an instance started for a vote of its epoch, for a block this node
    /// holds. Every instance that exists on some replica must reach the same
    /// outcome on every replica, so it is started here too, even if the block
    /// is already cemented. In the current epoch this node proposes the block
    /// as usual; in an epoch it has left it casts only its timeout vote and
    /// collects the certificates the others produce.
    pub fn insert_for_vote(&mut self, block: SavedBlock, epoch: ConsensusEpoch, now: Timestamp) {
        debug_assert!(epoch <= self.current_epoch);
        if self.stopped {
            return;
        }
        let id = ElectionId::new(block.qualified_root(), epoch);
        if self.roots.get(&id).is_some() {
            return;
        }
        if epoch < self.current_epoch {
            let slot = EpochSlot {
                account: block.account(),
                height: block.height(),
                epoch,
            };
            if self.slots.get(&slot).is_none() {
                *self.slots.get_or_default(&slot) = LocalSlotState::stale();
            }
            self.stats.stale_started += 1;
        } else {
            self.stats.started_for_vote += 1;
        }
        let request = AecInsertRequest::new_priority(block, BlockPriority::default());
        self.insert_new_election_in_epoch(request, epoch, now);
        if let Some(election) = self.roots.election_mut(&id) {
            election.transition_active();
        }
    }

    /// RAI: the epoch new elections are started in
    pub fn current_epoch(&self) -> ConsensusEpoch {
        self.current_epoch
    }

    pub fn set_current_epoch(&mut self, epoch: ConsensusEpoch) {
        self.current_epoch = epoch;
    }

    /// Kudzu: the votes to broadcast now for all elections, in round robin order
    pub fn kudzu_votes_due(&self) -> Vec<VoteTarget> {
        let mut targets = self.pending_kudzu_votes.clone();
        let empty = LocalSlotState::default();
        for election in self.roots.round_robin().map(|e| &e.election) {
            let slot = self.slots.get(&election.epoch_slot()).unwrap_or(&empty);
            // A settled instance has nothing left to say but its exit final
            // vote: the settled instances of the forks pile up over time and
            // this loop runs every tick under the AEC lock
            let due = if election.state() == ElectionState::Settled {
                election.kudzu_final_vote_due(slot).into_iter().collect()
            } else {
                election.kudzu_votes_due(slot)
            };
            targets.extend(due.into_iter().map(|(hash, kind)| VoteTarget {
                election: election.id(),
                winner: hash,
                vote_type: VoteType::from(kind),
            }));
        }
        targets
    }

    /// Kudzu: record the votes that are about to be handed to the vote
    /// generators and return those to generate. RAI: a first vote decided on
    /// before this node left the instance's epoch is dropped, the node
    /// abstains there instead.
    pub fn mark_kudzu_voted(&mut self, targets: Vec<VoteTarget>) -> Vec<VoteTarget> {
        let mut accepted = Vec::with_capacity(targets.len());
        for target in targets {
            self.pending_kudzu_votes
                .retain(|pending| *pending != target);
            let Some(election) = self.roots.election(&target.election) else {
                // The exit final vote of an election erased already
                accepted.push(target);
                continue;
            };
            let slot = self.slots.get_or_default(&election.epoch_slot());
            let kind = VoteKind::from(target.vote_type);
            // A first vote not cast before this node left the epoch is not cast
            // any more (it abstains instead); one cast before is re-broadcast
            if kind == VoteKind::First && slot.stale && slot.first_voted.is_none() {
                continue;
            }
            slot.mark_voted(target.winner, kind);
            accepted.push(target);
        }
        accepted
    }

    /// Kudzu: an election is erased as soon as it is finalized. Its exit final
    /// vote is kept so that the voter still broadcasts it.
    fn keep_exit_final_vote(&mut self, election: &Election) {
        // Only explicitly finalized elections: an implicitly finalized block is
        // already cemented, and its slot state has been dropped.
        if !cfg!(feature = "rai_protocol") || !election.certificates().is_finalized() {
            return;
        }
        let slot = self.slots.get_or_default(&election.epoch_slot());
        if let Some((hash, kind)) = election.kudzu_final_vote_due(slot) {
            slot.mark_voted(hash, kind);
            self.pending_kudzu_votes.push(VoteTarget {
                election: election.id(),
                winner: hash,
                vote_type: VoteType::from(kind),
            });
        }
    }

    /// Kudzu: the signed votes behind the certificates of the election of this
    /// block in the given epoch, if the election is terminated. RAI: the
    /// node's statements in an instance it finalized stay available after
    /// the election is gone.
    pub fn certificate_evidence(
        &self,
        hash: &BlockHash,
        epoch: ConsensusEpoch,
    ) -> Option<(ElectionId, CertificateEvidence)> {
        if let Some(election) = self.roots.election_for_block_in_epoch(hash, epoch) {
            let empty = LocalSlotState::default();
            let slot = self.slots.get(&election.epoch_slot()).unwrap_or(&empty);
            let evidence = election.certificate_evidence(slot)?;
            return Some((election.id(), evidence));
        }
        let instance = self.epoch_states.instance(hash, epoch)?;
        let statements = instance.statements();
        if statements.is_empty() {
            return None;
        }
        Some((
            ElectionId::new(instance.root.clone(), epoch),
            CertificateEvidence {
                statements,
                blocks: Vec::new(),
            },
        ))
    }

    /// Kudzu: is the election terminated and out of the priority buckets
    pub fn is_terminated(&self, id: &ElectionId) -> bool {
        self.roots.is_terminated(id)
    }

    pub fn slot_state(&self, slot: &EpochSlot) -> Option<&LocalSlotState> {
        self.slots.get(slot)
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
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

    pub fn find_bucket(&self, id: &ElectionId) -> Option<usize> {
        self.roots.find_bucket(id)
    }

    pub fn lowest_priority(&self, bucket_id: usize) -> Option<(ElectionId, TimePriority)> {
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
        if self.has_earlier_instance(&request.block.hash()) {
            return Err(AecInsertError::Duplicate);
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

        if self.recently_confirmed.root_exists(&root) {
            return Err(AecInsertError::RecentlyConfirmed);
        }
        Ok(())
    }

    fn try_upgrade_priority_election(
        &mut self,
        request: &AecInsertRequest,
    ) -> Result<bool, AecInsertError> {
        let (upgraded, previous_behavior) = self
            .roots
            .try_upgrade_to_priority_election(request, self.current_epoch);

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

    /// A new block always starts an election in the current epoch
    fn insert_new_election(&mut self, request: AecInsertRequest, now: Timestamp) {
        self.insert_new_election_in_epoch(request, self.current_epoch, now);
    }

    fn insert_new_election_in_epoch(
        &mut self,
        request: AecInsertRequest,
        epoch: ConsensusEpoch,
        now: Timestamp,
    ) {
        let root = request.block.qualified_root();
        let hash = request.block.hash();
        let election = Election::new(
            request.block,
            epoch,
            request.behavior,
            self.base_latency,
            now,
        );

        self.roots.insert(Entry {
            id: election.id(),
            election,
            priority: request.priority,
        });

        *self.count_by_behavior_mut(request.behavior) += 1;
        self.stats.started(request.behavior);
        self.notify(AecFact::ElectionStarted(hash, root));
    }

    /// A fork block joins the elections of its root in every epoch: each
    /// instance decides between the same candidates
    pub fn try_add_fork(&mut self, fork: &Block, fork_tally: Amount) -> bool {
        let root = fork.qualified_root();
        let ids: Vec<_> = self
            .roots
            .elections_for_root(&root)
            .map(|e| e.id())
            .collect();
        let mut any_added = false;
        for id in ids {
            any_added |= self.try_add_fork_to(&id, fork, fork_tally);
        }
        any_added
    }

    fn try_add_fork_to(&mut self, id: &ElectionId, fork: &Block, fork_tally: Amount) -> bool {
        let Some(entry) = self.roots.get_mut(id) else {
            return false;
        };

        let result = entry.election.try_add_fork(fork, fork_tally);
        let added = match result {
            AddForkResult::Added => {
                self.notify(AecFact::BlockAddedToElection(fork.hash()));
                true
            }
            AddForkResult::Replaced(removed) => {
                self.roots.vote_router.disconnect(&removed.hash(), id.epoch);
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
            self.roots.vote_router.connect(fork.hash(), id.clone());
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
        let current_size = self.roots.active_len() as i64;
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

    /// Whether the root has an election in any epoch
    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.roots.elections_for_root(root).next().is_some()
    }

    /// Whether the block is a candidate of an election of the current epoch.
    /// RAI: or of an instance of an earlier epoch, which keeps the schedulers
    /// from proposing the block again.
    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.roots
            .vote_router
            .election_id(block_hash, self.current_epoch)
            .is_some()
            || self.has_earlier_instance(block_hash)
    }

    /// RAI: whether the block has an instance of an earlier epoch. A block is
    /// proposed once: an instance that still runs decides it within the epoch
    /// it was proposed in, and one that ended undecided leaves it undecided.
    /// Only a vote of another replica opens an instance of a later epoch.
    pub fn has_earlier_instance(&self, block_hash: &BlockHash) -> bool {
        self.roots
            .vote_router
            .elections_of(block_hash)
            .any(|id| id.epoch < self.current_epoch)
    }

    /// Whether the block has an election that a priority activation could not
    /// upgrade any further, see `Election::maybe_upgrade_to`
    pub fn is_priority_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.roots
            .election_for_block_in_epoch(block_hash, self.current_epoch)
            .is_some_and(|e| {
                matches!(
                    e.behavior(),
                    ElectionBehavior::Priority | ElectionBehavior::Manual
                )
            })
            || self.has_earlier_instance(block_hash)
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

    pub fn election(&self, id: &ElectionId) -> Option<&Election> {
        self.roots.election(id)
    }

    /// The election of the newest epoch for this root
    pub fn election_for_root(&self, root: &QualifiedRoot) -> Option<&Election> {
        self.roots.latest_election_for_root(root)
    }

    /// The elections of all epochs for this root, ascending by epoch
    pub fn elections_for_root(&self, root: &QualifiedRoot) -> impl Iterator<Item = &Election> {
        self.roots.elections_for_root(root)
    }

    /// The election of the newest epoch this block is a candidate in
    pub fn election_for_block(&self, block_hash: &BlockHash) -> Option<&Election> {
        self.roots.election_for_block(block_hash)
    }

    /// Activates the elections of every epoch this block is a candidate in
    pub fn transition_active(&mut self, block_hash: &BlockHash) -> bool {
        let Some(root) = self
            .roots
            .election_for_block(block_hash)
            .map(|e| e.qualified_root().clone())
        else {
            return false;
        };
        for election in self.roots.elections_for_root_mut(&root) {
            if election.candidate_blocks().contains_key(block_hash) {
                election.transition_active();
            }
        }
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
                let bucket_vacancy = if self.roots.active_len() >= self.max_elections {
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
                let id = ElectionId::new(candidate.block.qualified_root(), self.current_epoch);
                if self.find_bucket(&id) == Some(candidate.bucket_id) {
                    self.stats.activate_failed_duplicate += 1;
                    continue;
                }

                if self.bucket_len(candidate.bucket_id) >= self.max_elections_per_bucket {
                    if self.erase_lowest_prio_election(candidate.bucket_id) {
                        self.stats.replaced += 1;
                    } else {
                        self.stats.over_capacity += 1;
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
        let Some(election) = self.roots.latest_election_for_root_mut(root) else {
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

    /// Erase the elections of all epochs of this root
    pub fn erase(&mut self, root: &QualifiedRoot) -> bool {
        let erased = self.roots.erase_root(root);
        let any = !erased.is_empty();
        for entry in erased {
            self.cleanup_election(entry);
        }
        any
    }

    pub fn erase_election(&mut self, id: &ElectionId) -> bool {
        let Some(entry) = self.roots.erase(id) else {
            return false;
        };
        self.cleanup_election(entry);
        true
    }

    /// Returns false if nothing could be evicted
    pub fn erase_lowest_prio_election(&mut self, bucket_id: usize) -> bool {
        // Kudzu: an election leaves the AEC only when it is finalized. Votes are
        // one-shot, so an evicted election would lose evidence, and the backlog scan
        // brings an evicted block back only seconds later. Candidates wait in the
        // scheduler instead, see `Bucket::available`.
        if cfg!(feature = "rai_protocol") {
            return false;
        }
        let Some((id, _)) = self.lowest_priority(bucket_id) else {
            return false;
        };
        if let Some(election) = self.roots.election(&id) {
            self.stats.evicted(election);
        }
        self.erase_election(&id)
    }

    fn cleanup_election(&mut self, entry: Entry) {
        let election = &entry.election;
        self.keep_exit_final_vote(election);
        if let Some(finalized) = election.certificates().finalized() {
            let slot = self
                .slots
                .get(&election.epoch_slot())
                .cloned()
                .unwrap_or_default();
            self.epoch_states.record_finalized(FinalizedInstance {
                root: election.qualified_root().clone(),
                account: election.account(),
                height: election.height(),
                epoch: election.epoch(),
                winner: finalized,
                candidates: election.candidate_blocks().keys().copied().collect(),
                slot,
            });
        }
        if cfg!(feature = "rai_protocol") {
            self.slots.remove(&election.epoch_slot());
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

        let root = confirmed_block.qualified_root();
        let Some(corresponding) = self.roots.latest_election_for_root_mut(&root) else {
            return ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::InactiveConfirmationHeight,
            );
        };

        // RAI: the block is cemented, but this replica's instances have not
        // reached their outcome yet. They keep running and collect the
        // certificates of the other replicas, so that every replica records the
        // same outcome per epoch.
        if cfg!(feature = "rai_protocol") {
            return ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::ActiveConfirmationHeight,
            );
        }

        let result = if corresponding.winner().hash() == confirmed_block.hash() {
            corresponding.force_confirm();
            corresponding.into_confirmed_election(now, ConfirmationType::ActiveConfirmationHeight)
        } else {
            corresponding.cancel();
            ConfirmedElection::new(
                confirmed_block.clone(),
                ConfirmationType::ActiveConfirmationHeight,
            )
        };
        // The height is decided, the elections of the other epochs are over too
        let latest = corresponding.epoch();
        for election in self.roots.elections_for_root_mut(&root) {
            if election.epoch() != latest {
                election.cancel();
            }
        }
        result
    }

    fn block_confirmed(&mut self, block: SavedBlock, election: ConfirmedElection) {
        self.stats.block_confirmations[election.confirmation_type as usize] += 1;
        // The height is finalized, nothing will be voted for it any more.
        // RAI: the instances of the height keep running until they reach their
        // outcome, their slot states go with them (`cleanup_election`).
        if !cfg!(feature = "rai_protocol") {
            self.slots.remove_slot(block.account(), block.height());
        }
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
            stats: &mut self.stats,
            observer: &self.observer,
            roots: &mut self.roots,
        };
        let result = apply_helper.apply_vote();
        for entry in result.confirmed {
            self.cleanup_election(entry);
        }
        self.count_decided(&result.decided, args.now);
        self.observe_epoch(&args);
        let mut per_block = result.per_block;
        // RAI: a vote of an epoch this node has not reached yet is not late, it
        // waits in the vote cache until this node gets there, also for a block
        // which was decided here in an earlier epoch
        if args.vote.epoch > self.current_epoch {
            for result in per_block.values_mut() {
                if matches!(result, Err(VoteError::Late)) {
                    *result = Err(VoteError::Indeterminate);
                }
            }
        }
        per_block
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
        for election in self.roots.elections_for_root_mut(root) {
            election.cancel();
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
            .leaf("terminated", self.roots.terminated_len(), 0)
            .leaf(
                "slots",
                self.slot_count(),
                size_of::<(EpochSlot, LocalSlotState)>(),
            )
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
            .leaf(
                "finalized_by_epoch",
                self.epoch_states.len(),
                size_of::<(BlockHash, ConsensusEpoch)>(),
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
    use rsnano_types::{PrivateKey, TimePriority, Vote, VoteDelivery};
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

    /// RAI: the same root is contested once per epoch. A block starts its
    /// election in the current epoch, and a vote only counts in the election
    /// of its own epoch. This node proposes a block once: with an instance of
    /// an earlier epoch, running or ended undecided, the block is not proposed
    /// again; only a vote of another replica opens the instance of a later epoch.
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn one_election_per_epoch() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();
        let root = block.qualified_root();
        let now = Timestamp::new_test_instance();
        let epoch1 = ConsensusEpoch::new(1);
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);

        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        container.set_current_epoch(epoch1);
        assert_eq!(
            container.insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            ),
            Err(AecInsertError::Duplicate)
        );
        assert!(container.is_active_hash(&block_hash));
        assert!(container.is_priority_active_hash(&block_hash));
        assert_eq!(container.len(), 1);

        // The epoch 0 instance times out: undecided, and still not proposed again
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::Abstain,
            ConsensusEpoch::ZERO,
            vec![block_hash],
        ));
        container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert!(container.is_active_hash(&block_hash));
        assert_eq!(
            container.insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            ),
            Err(AecInsertError::Duplicate)
        );
        assert_eq!(container.len(), 1);
        // A vote of epoch 1 opens that instance
        container.insert_for_vote(block, epoch1, now);

        assert_eq!(container.len(), 2);
        assert!(container.is_active_root(&root));
        let epochs: Vec<_> = container
            .elections_for_root(&root)
            .map(|e| e.epoch())
            .collect();
        assert_eq!(epochs, vec![ConsensusEpoch::ZERO, epoch1]);
        assert_eq!(
            container.election_for_block(&block_hash).unwrap().epoch(),
            epoch1
        );
        assert_eq!(
            container
                .election(&ElectionId::legacy(root.clone()))
                .unwrap()
                .epoch(),
            ConsensusEpoch::ZERO
        );

        // A vote of an epoch without an election counts nowhere
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::Final,
            ConsensusEpoch::new(2),
            vec![block_hash],
        ));
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(
            result.get(&block_hash),
            Some(&Err(VoteError::Indeterminate))
        );
        assert_eq!(container.len(), 2);

        // A final vote in epoch 1 confirms the epoch 1 election only
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::Final,
            epoch1,
            vec![block_hash],
        ));
        let result = container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert_eq!(result.get(&block_hash), Some(&Ok(())));
        assert_eq!(container.len(), 1);
        assert_eq!(
            container.election_for_block(&block_hash).unwrap().epoch(),
            ConsensusEpoch::ZERO
        );

        // Erasing by root erases every epoch
        assert!(container.erase(&root));
        assert_eq!(container.len(), 0);
        assert!(!container.is_active_hash(&block_hash));
    }

    /// left undecided
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn keeps_an_undecided_block_undecided_without_reproposing() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();
        let now = Timestamp::new_test_instance();
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);

        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::Abstain,
            ConsensusEpoch::ZERO,
            vec![block_hash],
        ));
        container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        container.set_current_epoch(ConsensusEpoch::new(1));

        assert!(container.is_active_hash(&block_hash));
        assert_eq!(
            container.insert(
                AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                now,
            ),
            Err(AecInsertError::Duplicate)
        );
        assert_eq!(container.len(), 1);
    }
    /// RAI: after enough decided elections the epoch advances; the undecided
    /// elections of the old epoch are started again in the new one and the
    /// finalized blocks are recorded per epoch
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epoch_advances_after_enough_decided_elections() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let decided = SavedBlock::new_test_instance_with_key(1);
        let undecided = SavedBlock::new_test_instance_with_key(2);
        let now = Timestamp::new_test_instance();
        for block in [&decided, &undecided] {
            container
                .insert(
                    AecInsertRequest::new_priority(
                        block.clone(),
                        BlockPriority::new_test_instance(),
                    ),
                    now,
                )
                .unwrap();
        }
        assert_eq!(container.current_epoch(), ConsensusEpoch::ZERO);

        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        let vote = test_final_vote(&rep_key, decided.hash());
        container.apply_vote(ApplyVoteArgs {
            vote: &vote.into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        let epoch1 = ConsensusEpoch::new(1);
        assert_eq!(container.current_epoch(), epoch1);
        assert_eq!(container.decided_in_current_epoch(), 0);
        assert!(container.finalized_in_epoch(&decided.hash(), ConsensusEpoch::ZERO));
        assert!(!container.finalized_in_epoch(&decided.hash(), epoch1));
        assert_eq!(
            container.finalized_by_epoch()[&ConsensusEpoch::ZERO].entries(),
            1
        );
        // The undecided election runs on in epoch 0, the decided one is gone
        let epochs: Vec<_> = container
            .elections_for_root(&undecided.qualified_root())
            .map(|e| e.epoch())
            .collect();
        assert_eq!(epochs, vec![ConsensusEpoch::ZERO]);
        assert_eq!(container.len(), 1);
        assert!(container.election_for_block(&decided.hash()).is_none());
        assert!(container.is_active_hash(&undecided.hash()));

        // This node proposed the block in epoch 0 and keeps voting there
        let old = ElectionId::legacy(undecided.qualified_root());
        let due = container.kudzu_votes_due();
        assert!(due.contains(&VoteTarget {
            election: old,
            winner: undecided.hash(),
            vote_type: VoteType::NonFinal,
        }));
        // plus the exit final vote of the finalized election
        assert!(due.contains(&VoteTarget {
            election: ElectionId::legacy(decided.qualified_root()),
            winner: decided.hash(),
            vote_type: VoteType::Final,
        }));
        assert_eq!(due.len(), 2);
    }

    /// RAI: once more than f of the weight votes in a later epoch this node
    /// follows without finishing its own count
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn follows_the_representatives_into_the_next_epoch() {
        let mut container = ActiveElectionsContainer::new(
            ActiveElectionsConfig {
                epoch_terminated_elections: 1000,
                ..Default::default()
            },
            Duration::from_secs(1),
        );
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        container
            .insert(
                AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();

        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        let epoch1 = ConsensusEpoch::new(1);
        let vote = Arc::new(Vote::new_in_epoch(
            &rep_key,
            VoteKind::First,
            epoch1,
            vec![block.hash()],
        ));
        container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote, VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        assert_eq!(container.current_epoch(), epoch1);
        // The open instance runs on in epoch 0
        assert!(
            container
                .election(&ElectionId::new(block.qualified_root(), epoch1))
                .is_none()
        );
        assert!(
            container
                .election(&ElectionId::legacy(block.qualified_root()))
                .is_some()
        );
    }

    /// RAI: a vote for a block without an instance in the vote's epoch starts
    /// that instance. In an epoch this node has left it casts only a timeout
    /// vote there, in the current epoch it proposes the block
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn instances_are_started_for_votes() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let now = Timestamp::new_test_instance();
        let epoch1 = ConsensusEpoch::new(1);
        container.set_current_epoch(epoch1);

        container.insert_for_vote(block.clone(), ConsensusEpoch::ZERO, now);
        // A second start of the same instance changes nothing
        container.insert_for_vote(block.clone(), ConsensusEpoch::ZERO, now);
        container.insert_for_vote(block.clone(), epoch1, now);

        let stale = ElectionId::legacy(block.qualified_root());
        let current = ElectionId::new(block.qualified_root(), epoch1);
        assert_eq!(
            container.election(&stale).unwrap().state(),
            ElectionState::Active
        );
        assert_eq!(container.len(), 2);
        let due = container.kudzu_votes_due();
        assert!(due.contains(&VoteTarget {
            election: stale,
            winner: block.hash(),
            vote_type: VoteType::Abstain,
        }));
        assert!(due.contains(&VoteTarget {
            election: current,
            winner: block.hash(),
            vote_type: VoteType::NonFinal,
        }));
        assert_eq!(due.len(), 2);
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

    #[test]
    fn kudzu_first_vote_is_due_for_a_new_election_and_recorded_per_slot() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let request =
            AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance());
        container
            .insert(request, Timestamp::new_test_instance())
            .unwrap();

        let due = container.kudzu_votes_due();
        assert_eq!(
            due,
            vec![VoteTarget {
                election: ElectionId::legacy(block.qualified_root()),
                winner: block.hash(),
                vote_type: VoteType::NonFinal,
            }]
        );

        container.mark_kudzu_voted(due.clone());
        let slot = container
            .slot_state(&EpochSlot {
                account: block.account(),
                height: block.height(),
                epoch: ConsensusEpoch::ZERO,
            })
            .unwrap();
        assert_eq!(slot.first_voted, Some(block.hash()));
        assert_eq!(container.slot_count(), 1);

        // Nothing new is decided, the first vote is only re-broadcast
        assert_eq!(container.kudzu_votes_due(), due);
    }

    #[test]
    fn kudzu_slot_state_is_dropped_when_the_height_is_confirmed() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let request =
            AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance());
        let now = Timestamp::new_test_instance();
        container.insert(request, now).unwrap();
        let due = container.kudzu_votes_due();
        container.mark_kudzu_voted(due.clone());
        assert_eq!(container.slot_count(), 1);

        container.confirm_dependent_elections(vec![(block.clone(), None)], now);

        // RAI: the instance keeps running until it reaches its outcome, its
        // slot state goes when the election does
        if cfg!(feature = "rai_protocol") {
            assert_eq!(container.slot_count(), 1);
            container.erase(&block.qualified_root());
        }
        assert_eq!(container.slot_count(), 0);
    }

    /// Legacy never confirms on a non-final vote, so this only applies to Kudzu
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn kudzu_exit_final_vote_of_a_fast_finalized_election_is_kept() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();
        let root = block.qualified_root();
        let request = AecInsertRequest::new_priority(block, BlockPriority::new_test_instance());
        let now = Timestamp::new_test_instance();
        container.insert(request, now).unwrap();
        let first = container.kudzu_votes_due();
        container.mark_kudzu_voted(first.clone());

        // A single first vote with all the weight fast finalizes and erases the election
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::MAX);
        let vote = Arc::new(Vote::new(
            &rep_key,
            rsnano_types::UnixMillisTimestamp::new(1000),
            0,
            vec![block_hash],
        ));
        let received = ReceivedVote::new(vote, VoteDelivery::Direct, None);
        container.apply_vote(ApplyVoteArgs {
            vote: &received.into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });
        assert!(container.election_for_block(&block_hash).is_none());

        let expected = VoteTarget {
            election: ElectionId::legacy(root),
            winner: block_hash,
            vote_type: VoteType::Final,
        };
        assert_eq!(container.kudzu_votes_due(), vec![expected.clone()]);
        container.mark_kudzu_voted(vec![expected]);
        assert!(container.kudzu_votes_due().is_empty());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn terminated_election_leaves_the_buckets_and_hands_out_its_votes() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let block_hash = block.hash();
        let root = block.qualified_root();
        let now = Timestamp::new_test_instance();
        container
            .insert(
                AecInsertRequest::new_priority(block, BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();
        let vacancy_before = container.vacancy();
        let id = ElectionId::legacy(root.clone());
        assert!(!container.is_terminated(&id));
        assert!(
            container
                .certificate_evidence(&block_hash, ConsensusEpoch::ZERO)
                .is_none()
        );

        // 70%: notarization certificate, no fast finalization
        let rep_key = PrivateKey::from(1);
        let mut rep_weights = RepWeights::default();
        rep_weights.put(rep_key.public_key(), Amount::nano(70_000_000));
        let vote = Arc::new(Vote::new(
            &rep_key,
            rsnano_types::UnixMillisTimestamp::new(1000),
            0,
            vec![block_hash],
        ));
        container.apply_vote(ApplyVoteArgs {
            vote: &ReceivedVote::new(vote.clone(), VoteDelivery::Direct, None).into(),
            rep_weights: &rep_weights,
            quorum_snapshot: &QuorumSnapshot::new_test_instance(),
            now,
        });

        assert!(container.is_terminated(&id));
        // It no longer takes capacity but is still there and iterated
        assert_eq!(container.vacancy(), vacancy_before + 1);
        assert_eq!(container.len(), 1);
        assert_eq!(container.iter_round_robin().count(), 1);
        assert!(container.election_for_block(&block_hash).is_some());

        let (served, evidence) = container
            .certificate_evidence(&block_hash, ConsensusEpoch::ZERO)
            .unwrap();
        assert_eq!(served, id);
        // The election of another epoch has no evidence
        assert!(
            container
                .certificate_evidence(&block_hash, ConsensusEpoch::new(1))
                .is_none()
        );
        // This node never voted here, so it only hands out the candidate
        assert!(evidence.statements.is_empty());
        assert_eq!(evidence.blocks.len(), 1);
        assert_eq!(evidence.blocks[0].hash(), block_hash);

        container.mark_kudzu_voted(vec![VoteTarget {
            election: id.clone(),
            winner: block_hash,
            vote_type: VoteType::NonFinal,
        }]);
        let (_, evidence) = container
            .certificate_evidence(&block_hash, ConsensusEpoch::ZERO)
            .unwrap();
        assert_eq!(
            evidence.statements,
            vec![(VoteKind::First, vec![block_hash])]
        );

        // Erasing it keeps the accounting consistent
        assert!(container.erase(&root));
        assert_eq!(container.len(), 0);
        assert_eq!(container.vacancy(), vacancy_before + 1);
    }

    #[test]
    fn kudzu_votes_for_unknown_roots_are_not_recorded() {
        let mut container = ActiveElectionsContainer::default();
        container.mark_kudzu_voted(vec![VoteTarget {
            election: ElectionId::new_test_instance(),
            winner: BlockHash::from(1),
            vote_type: VoteType::NonFinal,
        }]);
        assert_eq!(container.slot_count(), 0);
    }

    /// Kudzu: a running election is never evicted for a higher priority block
    #[test]
    fn evict_lowest_priority_election() {
        let mut container = ActiveElectionsContainer::default();
        let block = SavedBlock::new_test_instance();
        let request =
            AecInsertRequest::new_priority(block.clone(), BlockPriority::new_test_instance());
        container
            .insert(request, Timestamp::new_test_instance())
            .unwrap();
        let bucket = container
            .find_bucket(&ElectionId::legacy(block.qualified_root()))
            .unwrap();

        let evicted = container.erase_lowest_prio_election(bucket);

        assert_eq!(evicted, !cfg!(feature = "rai_protocol"));
        assert_eq!(container.len(), if evicted { 0 } else { 1 });
    }

    #[test]
    fn priority_active_hash_only_for_elections_that_cannot_be_upgraded() {
        let mut container = ActiveElectionsContainer::default();
        let now = Timestamp::new_test_instance();
        let priority = SavedBlock::new_test_instance_with_key(1);
        let hinted = SavedBlock::new_test_instance_with_key(2);
        container
            .insert(
                AecInsertRequest::new_priority(
                    priority.clone(),
                    BlockPriority::new_test_instance(),
                ),
                now,
            )
            .unwrap();
        container
            .insert(
                AecInsertRequest::new_hinted(hinted.clone(), BlockPriority::new_test_instance()),
                now,
            )
            .unwrap();

        assert!(container.is_priority_active_hash(&priority.hash()));
        assert!(!container.is_priority_active_hash(&hinted.hash()));
        assert!(!container.is_priority_active_hash(&BlockHash::from(3)));
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
