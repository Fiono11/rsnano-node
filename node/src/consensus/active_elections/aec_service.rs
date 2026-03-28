use std::{
    collections::HashMap,
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::Duration,
};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{
    Amount, Block, BlockHash, PublicKey, QualifiedRoot, SavedBlock, TimePriority, VoteError,
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
use crate::consensus::election::{ConfirmedElection, ElectionBehavior, VoteType};

pub struct AecService {
    inner: Arc<RwLock<ActiveElectionsContainer>>,
}

impl AecService {
    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ActiveElectionsContainer::new(
                config,
                base_latency,
            ))),
        }
    }

    pub fn new_null() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ActiveElectionsContainer::default())),
        }
    }

    pub fn read(&self) -> RwLockReadGuard<'_, ActiveElectionsContainer> {
        self.inner.read().unwrap()
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, ActiveElectionsContainer> {
        self.inner.write().unwrap()
    }

    // --- Read forwarding ---

    pub fn max_len(&self) -> usize {
        self.inner.read().unwrap().max_len()
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().is_empty()
    }

    pub fn is_active_root(&self, root: &QualifiedRoot) -> bool {
        self.inner.read().unwrap().is_active_root(root)
    }

    pub fn is_active_hash(&self, block_hash: &BlockHash) -> bool {
        self.inner.read().unwrap().is_active_hash(block_hash)
    }

    pub fn was_recently_confirmed(&self, block_hash: &BlockHash) -> bool {
        self.inner
            .read()
            .unwrap()
            .was_recently_confirmed(block_hash)
    }

    pub fn count_by_behavior(&self, behavior: ElectionBehavior) -> usize {
        self.inner.read().unwrap().count_by_behavior(behavior)
    }

    pub fn bucket_len(&self, bucket_id: usize) -> usize {
        self.inner.read().unwrap().bucket_len(bucket_id)
    }

    pub fn find_bucket(&self, root: &QualifiedRoot) -> Option<usize> {
        self.inner.read().unwrap().find_bucket(root)
    }

    pub fn lowest_priority(&self, bucket_id: usize) -> Option<(QualifiedRoot, TimePriority)> {
        self.inner.read().unwrap().lowest_priority(bucket_id)
    }

    pub fn vacancy(&self) -> i64 {
        self.inner.read().unwrap().vacancy()
    }

    pub fn info(&self) -> ActiveElectionsInfo {
        self.inner.read().unwrap().info()
    }

    // --- Write forwarding ---

    pub fn set_observer(&self, observer: Sender<AecFact>) {
        self.inner.write().unwrap().set_observer(observer)
    }

    pub fn insert(&self, request: AecInsertRequest, now: Timestamp) -> Result<(), AecInsertError> {
        self.inner.write().unwrap().insert(request, now)
    }

    pub fn try_add_fork(&self, fork: &Block, fork_tally: Amount) -> bool {
        self.inner.write().unwrap().try_add_fork(fork, fork_tally)
    }

    pub fn set_last_voted(&self, root: &QualifiedRoot, vote_type: VoteType, timestamp: Timestamp) {
        self.inner
            .write()
            .unwrap()
            .set_last_voted(root, vote_type, timestamp)
    }

    pub fn apply_vote<'a>(
        &self,
        args: ApplyVoteArgs<'a>,
    ) -> HashMap<BlockHash, Result<(), VoteError>> {
        self.inner.write().unwrap().apply_vote(args)
    }

    pub fn transition_time(&self, now: Timestamp) {
        self.inner.write().unwrap().transition_time(now)
    }

    pub fn transition_active(&self, block_hash: &BlockHash) -> bool {
        self.inner.write().unwrap().transition_active(block_hash)
    }

    pub fn remove_votes<'a>(
        &self,
        root: &QualifiedRoot,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        self.inner.write().unwrap().remove_votes(root, voters)
    }

    pub fn erase_ended_elections(&self) {
        self.inner.write().unwrap().erase_ended_elections()
    }

    pub fn erase(&self, root: &QualifiedRoot) -> bool {
        self.inner.write().unwrap().erase(root)
    }

    pub fn erase_lowest_prio_election(&self, bucket_id: usize) {
        self.inner
            .write()
            .unwrap()
            .erase_lowest_prio_election(bucket_id)
    }

    pub fn confirm_dependent_elections(
        &self,
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>)>,
        now: Timestamp,
    ) {
        self.inner
            .write()
            .unwrap()
            .confirm_dependent_elections(confirmed, now)
    }

    pub fn remove_recently_confirmed(&self, block_hash: &BlockHash) {
        self.inner
            .write()
            .unwrap()
            .remove_recently_confirmed(block_hash)
    }

    pub fn set_cooldown(&self, cool_down: bool, reason: AecCooldownReason) {
        self.inner.write().unwrap().set_cooldown(cool_down, reason)
    }

    pub fn cancel(&self, root: &QualifiedRoot) {
        self.inner.write().unwrap().cancel(root)
    }

    pub fn cancel_all(&self) {
        self.inner.write().unwrap().cancel_all()
    }

    pub fn clear_recently_confirmed(&self) {
        self.inner.write().unwrap().clear_recently_confirmed()
    }

    pub fn stop(&self) {
        self.inner.write().unwrap().stop()
    }

    pub fn force_confirm(&self, block_hash: &BlockHash, now: Timestamp) {
        self.inner.write().unwrap().force_confirm(block_hash, now)
    }

    pub fn simulate_event(&self, event: AecFact) {
        self.inner.read().unwrap().simulate_event(event)
    }
}

impl StatsSource for AecService {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.inner.read().unwrap().collect_stats(result)
    }
}

impl ContainerInfoProvider for AecService {
    fn container_info(&self) -> ContainerInfo {
        self.inner.read().unwrap().container_info()
    }
}
