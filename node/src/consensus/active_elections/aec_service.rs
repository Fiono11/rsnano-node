use std::{collections::HashMap, sync::RwLock, time::Duration};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Account, Amount, Block, BlockHash, PublicKey, QualifiedRoot, Root, SavedBlock, VoteError,
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

#[cfg(feature = "rai_protocol")]
#[derive(Default)]
pub struct EpochDrainStatus {
    pub active: usize,
    pub missing: usize,
    pub no_votes: usize,
    pub awaiting_second_look: usize,
    pub second_look: usize,
    pub quorum: usize,
    pub terminated: usize,
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
    pub fn insert_now(&self, request: AecInsertRequest) -> Result<(), AecInsertError> {
        self.aec.write().unwrap().insert(request, self.clock.now())
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
        confirmed: Vec<(SavedBlock, Option<ConfirmedElection>, u64)>,
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
    pub fn pending_for_epoch(&self, epoch: u64) -> Vec<(rsnano_types::SlotRoot, BlockHash)> {
        let mut result: Vec<_> = self
            .aec
            .read()
            .unwrap()
            .iter_round_robin()
            .filter(|election| election.qualified_root().epoch == epoch && !election.is_final())
            .map(|election| (election.qualified_root().slot(), election.winner().hash()))
            .collect();
        result.sort_unstable_by_key(|(slot, _)| *slot);
        result.dedup_by_key(|(slot, _)| *slot);
        result
    }

    #[cfg(feature = "rai_protocol")]
    pub fn finalized_for_epoch(&self, epoch: u64) -> HashMap<rsnano_types::SlotRoot, BlockHash> {
        self.aec.read().unwrap().finalized_for_epoch(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn clear_finalized_for_epoch(&self, epoch: u64) {
        self.aec.write().unwrap().clear_finalized_for_epoch(epoch);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn finalized_before_epoch(
        &self,
        epoch: u64,
    ) -> std::collections::HashSet<rsnano_types::SlotRoot> {
        self.aec.read().unwrap().finalized_before_epoch(epoch)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn seal_finalized_epoch(&self, epoch: u64) {
        self.aec.write().unwrap().seal_finalized_epoch(epoch);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn merge_finalized_for_epoch(
        &self,
        epoch: u64,
        finalized: HashMap<rsnano_types::SlotRoot, BlockHash>,
    ) {
        self.aec
            .write()
            .unwrap()
            .merge_finalized_for_epoch(epoch, finalized);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn replace_finalized_for_epoch(
        &self,
        epoch: u64,
        finalized: HashMap<rsnano_types::SlotRoot, BlockHash>,
    ) {
        self.aec
            .write()
            .unwrap()
            .replace_finalized_for_epoch(epoch, finalized);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn current_epoch(&self) -> u64 {
        self.aec.read().unwrap().current_epoch()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn advance_epoch(&self) -> u64 {
        self.aec.write().unwrap().advance_epoch()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn install_epoch_cut(
        &self,
        epoch: u64,
        cut: std::collections::HashSet<rsnano_types::SlotRoot>,
    ) -> usize {
        self.aec
            .write()
            .unwrap()
            .install_epoch_cut(epoch, cut, self.clock.now())
    }

    #[cfg(feature = "rai_protocol")]
    pub fn clear_epoch_cut(&self, epoch: u64) {
        self.aec.write().unwrap().clear_epoch_cut(epoch);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn remove_epoch_elections(&self, epoch: u64) {
        self.aec.write().unwrap().remove_epoch_elections(epoch);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn epoch_drain_status(
        &self,
        epoch: u64,
        slots: &std::collections::HashSet<rsnano_types::SlotRoot>,
    ) -> EpochDrainStatus {
        let guard = self.aec.read().unwrap();
        let mut status = EpochDrainStatus::default();
        let finalized = guard.finalized_for_epoch(epoch);
        for slot in slots {
            let Some(election) = guard.election_for_root(&slot.with_epoch(epoch)) else {
                if !finalized.contains_key(slot) {
                    status.missing += 1;
                }
                continue;
            };
            status.active += 1;
            if election.is_terminated() {
                status.terminated += 1;
            }
            if election.has_quorum() {
                status.quorum += 1;
            } else if election.second_look_targets().next().is_some() {
                status.second_look += 1;
            } else if election.vote_count() == 0 {
                status.no_votes += 1;
            } else {
                status.awaiting_second_look += 1;
            }
        }
        status
    }

    /// Returns the deterministic outcome of every cut election once all of them have
    /// terminated. Timeout-only elections are deliberately absent from the returned map.
    #[cfg(feature = "rai_protocol")]
    pub fn terminated_cut_values(
        &self,
        epoch: u64,
        slots: &std::collections::HashSet<rsnano_types::SlotRoot>,
    ) -> Option<HashMap<rsnano_types::SlotRoot, BlockHash>> {
        let guard = self.aec.read().unwrap();
        let finalized = guard.finalized_for_epoch(epoch);
        let mut values = HashMap::new();
        for slot in slots {
            if let Some(hash) = finalized.get(slot) {
                values.insert(*slot, *hash);
                continue;
            }
            let election = guard.election_for_root(&slot.with_epoch(epoch))?;
            if !election.is_terminated() {
                return None;
            }
            if let Some(hash) = election.notarized_value() {
                values.insert(*slot, hash);
            }
        }
        Some(values)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn missing_for_epoch(
        &self,
        epoch: u64,
        slots: &std::collections::HashSet<rsnano_types::SlotRoot>,
    ) -> Vec<rsnano_types::SlotRoot> {
        let guard = self.aec.read().unwrap();
        let finalized = guard.finalized_for_epoch(epoch);
        slots
            .iter()
            .filter(|slot| {
                guard.election_for_root(&slot.with_epoch(epoch)).is_none()
                    && !finalized.contains_key(slot)
            })
            .copied()
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn final_vote_recovery_targets(
        &self,
        epoch: u64,
        slots: &std::collections::HashSet<rsnano_types::SlotRoot>,
    ) -> Vec<(BlockHash, Root)> {
        let guard = self.aec.read().unwrap();
        let mut targets: Vec<_> = guard
            .iter_round_robin()
            .filter(|election| {
                election.qualified_root().epoch == epoch
                    && slots.contains(&election.qualified_root().slot())
                    && election.has_quorum()
                    && !election.is_confirmed()
            })
            .map(|election| (election.winner().hash(), election.winner().root()))
            .collect();
        targets.sort_unstable();
        targets.dedup();
        targets
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
