use std::{collections::HashMap, sync::RwLock, time::Duration};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Account, Amount, Block, BlockHash, PublicKey, QualifiedRoot, SavedBlock, VoteError,
};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};

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

impl AecService {
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::new(config, base_latency)),
            clock: SteadyClock::default(),
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

    #[cfg(feature = "rai_protocol")]
    pub fn epoch_slots(&self, epoch: u64) -> Vec<(QualifiedRoot, BlockHash)> {
        self.aec.read().unwrap().epoch_slots(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn finalized_epoch_slots(&self, epoch: u64) -> Vec<(QualifiedRoot, BlockHash)> {
        self.aec.read().unwrap().finalized_epoch_slots(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn epoch_slot_outcome(&self, root: &QualifiedRoot) -> Option<Option<BlockHash>> {
        self.aec.read().unwrap().epoch_slot_outcome(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn epoch_slot_finalized_or_timed_out(&self, root: &QualifiedRoot) -> bool {
        self.aec
            .read()
            .unwrap()
            .epoch_slot_finalized_or_timed_out(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn begin_epoch_one(&self, baseline: HashMap<Account, u64>) {
        self.aec.write().unwrap().begin_epoch_one(baseline);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn advance_epoch(&self, baseline: HashMap<Account, u64>) -> u64 {
        self.aec.write().unwrap().advance_epoch(baseline)
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
    pub fn insert_for_epoch(
        &self,
        request: AecInsertRequest,
        now: Timestamp,
        epoch: u64,
    ) -> Result<(), AecInsertError> {
        self.aec
            .write()
            .unwrap()
            .insert_for_epoch(request, now, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn has_election_for_epoch(&self, hash: &BlockHash, epoch: u64) -> bool {
        self.aec.read().unwrap().has_election_for_epoch(hash, epoch)
    }

    pub fn try_add_fork(&self, fork: &Block, fork_tally: Amount) -> bool {
        self.aec.write().unwrap().try_add_fork(fork, fork_tally)
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

    pub fn transition_active(&self, block_hash: &BlockHash) -> bool {
        self.aec.write().unwrap().transition_active(block_hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn transition_active_root(&self, root: &QualifiedRoot) -> bool {
        self.aec.write().unwrap().transition_active_root(root)
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

    #[cfg(feature = "rai_protocol")]
    pub fn exclude_by_cut(&self, root: &QualifiedRoot) -> bool {
        self.aec.write().unwrap().exclude_by_cut(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn suppress_epoch_votes(&self, epoch: u64) {
        self.aec.write().unwrap().suppress_epoch_votes(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn resume_cut_votes(
        &self,
        epoch: u64,
        included: &std::collections::HashSet<QualifiedRoot>,
    ) {
        self.aec.write().unwrap().resume_cut_votes(epoch, included)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_cemented_outcome(&self, block: &SavedBlock, source_epoch: Option<u64>) -> bool {
        self.aec
            .write()
            .unwrap()
            .apply_cemented_outcome(block, source_epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn belongs_to_epoch(&self, block: &SavedBlock, epoch: u64) -> bool {
        self.aec.read().unwrap().belongs_to_epoch(block, epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_rolled_back_outcome(&self, root: &QualifiedRoot) -> bool {
        self.aec.write().unwrap().apply_rolled_back_outcome(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_record_outcome(&self, root: &QualifiedRoot, hash: BlockHash) -> bool {
        self.aec.write().unwrap().apply_record_outcome(root, hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn apply_rolled_back_block(&self, hash: &BlockHash) -> bool {
        self.aec.write().unwrap().apply_rolled_back_block(hash)
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
