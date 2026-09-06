use std::{collections::HashMap, sync::RwLock, time::Duration};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Account, Amount, Block, BlockHash, MaybeSavedBlock, PublicKey, QualifiedRoot, SavedBlock,
    VoteError,
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
    election::{ConfirmedElection, Election, ElectionBehavior, ElectionState, VoteType},
};

pub struct AecService {
    aec: RwLock<ActiveElectionsContainer>,
    clock: SteadyClock,
    #[cfg(feature = "rai_protocol")]
    recovery_candidates: RwLock<HashMap<(u64, BlockHash), Block>>,
}

#[cfg(feature = "rai_protocol")]
#[derive(Default)]
pub struct EpochDrainStatus {
    pub finalized: usize,
    pub active: usize,
    pub missing: usize,
    pub no_votes: usize,
    pub awaiting_second_look: usize,
    pub second_look: usize,
    pub quorum: usize,
    pub terminated: usize,
}

impl AecService {
    #[cfg(feature = "rai_protocol")]
    pub fn unfinalized_election_slots(&self) -> Vec<rsnano_types::SlotRoot> {
        let guard = self.aec.read().unwrap();
        let mut slots: Vec<_> = guard.iter_round_robin()
            .filter(|election| !election.is_confirmed()
                && guard.earliest_finalization(election.qualified_root().slot())
                    .is_none_or(|(epoch, _)| epoch > election.qualified_root().epoch))
            .map(|election| election.qualified_root().slot()).collect();
        slots.sort_unstable();
        slots.dedup();
        slots
    }

    #[cfg(feature = "rai_protocol")]
    pub fn observed_cut_values(
        &self, epoch: u64, slots: &std::collections::HashSet<rsnano_types::SlotRoot>,
    ) -> HashMap<rsnano_types::SlotRoot, BlockHash> {
        let guard = self.aec.read().unwrap();
        let finalized = guard.finalized_for_epoch(epoch);
        slots.iter().filter_map(|slot| {
            finalized.get(slot).copied().or_else(|| {
                guard.election_for_root(&slot.with_epoch(epoch))
                    .filter(|election| election.is_terminated())
                    .and_then(|election| election.notarized_value())
            }).map(|hash| (*slot, hash))
        }).collect()
    }

    /// Prefer the earliest finalization; otherwise recover the earliest active instance.
    #[cfg(feature = "rai_protocol")]
    pub fn earliest_election(&self, slot: rsnano_types::SlotRoot) -> Option<(u64, Option<BlockHash>)> {
        let guard = self.aec.read().unwrap();
        guard.earliest_finalization(slot).map(|(epoch, hash)| (epoch, Some(hash)))
            .or_else(|| guard.iter_round_robin()
                .filter(|e| e.qualified_root().slot() == slot)
                .map(|e| (e.qualified_root().epoch, None))
                .min_by_key(|(epoch, _)| *epoch))
    }

    #[cfg(feature = "rai_protocol")]
    pub fn is_finalized_in_epoch(&self, root: &QualifiedRoot, hash: &BlockHash) -> bool {
        self.aec.read().unwrap().earliest_finalization(root.slot()) == Some((root.epoch, *hash))
    }

    #[cfg(feature = "rai_protocol")]
    pub fn observe_block_receipt(&self, hash: BlockHash) {
        self.aec.write().unwrap().observe_block_receipt(hash);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn mark_epoch_cut_decided(&self, epoch: u64) {
        self.aec.write().unwrap().mark_epoch_cut_decided(epoch);
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn second_look_eligible(&self, root: &QualifiedRoot, hash: &BlockHash) -> bool {
        self.aec.read().unwrap().election_for_root(root).is_some_and(|election| {
            !election.is_confirmed()
                && election.second_look_targets().any(|target| target == *hash)
        })
    }
    #[cfg(feature = "rai_protocol")]
    pub fn is_active_hash_in_epoch(&self, epoch: u64, hash: &BlockHash) -> bool {
        self.aec.read().unwrap().is_active_hash_in_epoch(epoch, hash)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn finalization_epoch(&self, slot: rsnano_types::SlotRoot, proposed: u64) -> u64 {
        self.aec.read().unwrap().finalization_epoch(slot, proposed)
    }
    #[cfg(feature = "rai_protocol")]
    pub fn finalized_hash_for_root(&self, epoch: u64, root: &rsnano_types::Root) -> Option<BlockHash> {
        self.aec.read().unwrap().finalized_for_epoch(epoch).into_iter()
            .find_map(|(slot, hash)| (slot.root == *root).then_some(hash))
    }
    #[cfg(feature = "rai_protocol")]
    pub fn epoch_recovery_diagnostics(&self, epoch: u64) -> Vec<String> {
        let guard = self.aec.read().unwrap();
        guard.iter_round_robin().map(|e| format!(
            "root={} epoch={} state={:?} terminated={} confirmed={} first={} final={} candidates={:?} phases={:?}",
            e.winner().root(), e.qualified_root().epoch, e.state(), e.is_terminated(), e.is_confirmed(),
            e.votes().values().filter(|v| v.first.is_some()).count(),
            e.votes().values().filter(|v| v.final_vote.is_some()).count(),
            e.candidate_blocks().keys().collect::<Vec<_>>(),
            e.votes().iter().map(|(voter, vote)| (voter, vote.first, vote.final_vote)).collect::<Vec<_>>()
        )).chain((0..=guard.current_epoch().max(epoch)).flat_map(|record_epoch| {
            guard.finalized_for_epoch(record_epoch).into_iter()
                .map(move |(slot, hash)| format!("recorded root={} hash={} epoch={}", slot.root, hash, record_epoch))
        })).collect()
    }
    #[cfg(feature = "rai_protocol")]
    pub fn missing_final_votes(&self, epoch: u64, committee: &std::collections::HashSet<rsnano_types::PublicKey>) -> Vec<(rsnano_types::PublicKey, rsnano_types::Root)> {
        let guard = self.aec.read().unwrap();
        let finalized = guard.finalized_for_epoch(epoch);
        let mut result = Vec::new();
        for election in guard.iter_round_robin().filter(|e| e.qualified_root().epoch == epoch && !finalized.contains_key(&e.qualified_root().slot())) {
            if let Some(block) = election.candidate_blocks().values().next() {
                for voter in committee {
                    if election.votes().get(voter).is_none_or(|vote| vote.final_vote.is_none()) {
                        result.push((*voter, block.root()));
                    }
                }
            }
        }
        result
    }
    #[cfg(feature = "rai_protocol")]
    pub fn election_candidate(&self, epoch: u64, root: &rsnano_types::Root) -> Option<MaybeSavedBlock> {
        self.aec.read().unwrap().iter_round_robin()
            .filter(|election| election.qualified_root().epoch == epoch)
            .flat_map(|election| election.candidate_blocks().values())
            .find(|block| block.root() == *root).cloned()
    }
    #[cfg(feature = "rai_protocol")]
    pub fn candidate_block(&self, epoch: u64, hash: &BlockHash) -> Option<MaybeSavedBlock> {
        self.aec
            .read()
            .unwrap()
            .iter_round_robin()
            .filter(|election| election.qualified_root().epoch == epoch)
            .find_map(|election| election.candidate_blocks().get(hash).cloned())
            .or_else(|| {
                self.recovery_candidates
                    .read()
                    .unwrap()
                    .get(&(epoch, *hash))
                    .cloned()
                    .map(MaybeSavedBlock::Unsaved)
            })
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_vote_recovery(&self, block: Block, epoch: u64) -> bool {
        let inserted = self.aec.write().unwrap().insert_vote_recovery(
            block.clone(), epoch, self.clock.now(),
        );
        self.recovery_candidates.write().unwrap().insert((epoch, block.hash()), block);
        inserted
    }

    #[cfg(feature = "rai_protocol")]
    pub fn insert_cut_recovery(&self, block: Block) -> bool {
        let hash = block.hash();
        let mut guard = self.aec.write().unwrap();
        let inserted = guard.insert_cut_recovery(block.clone(), self.clock.now());
        let epoch = guard
            .election_for_block(&hash)
            .map(|election| election.qualified_root().epoch)
            .unwrap_or_else(|| guard.current_epoch());
        drop(guard);
        self.recovery_candidates
            .write()
            .unwrap()
            .insert((epoch, hash), block);
        inserted
    }

    #[cfg(feature = "rai_protocol")]
    pub fn recovery_vote_type(&self, epoch: u64, hash: &BlockHash) -> Option<VoteType> {
        let guard = self.aec.read().unwrap();
        let election = guard
            .election_for_block(hash)
            .filter(|election| election.qualified_root().epoch == epoch)?;
        if election.second_look_targets().any(|target| target == *hash) {
            // Every PR must originate its own second-look vote for every eligible candidate,
            // even if votes from other representatives already formed both certificates and
            // terminated this replica's election before its generator ran.
            return Some(VoteType::NonFinal);
        }
        if !election.has_quorum() {
            // A vote can arrive before its candidate block and be cached.  If that happened to
            // a first vote, asking only for second-look votes can never recreate the missing
            // phase-one evidence.  Recover first votes until this replica can derive at least
            // one second-look target; subsequent requests then recover the second-look phase.
            if election.second_look_targets().next().is_some()
                && election.candidate_blocks().contains_key(hash)
            {
                Some(VoteType::NonFinal)
            } else {
                Some(VoteType::First)
            }
        } else if election.has_quorum() && election.winner().hash() == *hash {
            Some(VoteType::Final)
        } else if election.winner().hash() == *hash {
            Some(VoteType::First)
        } else {
            None
        }
    }

    pub fn new(config: ActiveElectionsConfig, base_latency: Duration) -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::new(config, base_latency)),
            clock: SteadyClock::default(),
            #[cfg(feature = "rai_protocol")]
            recovery_candidates: Default::default(),
        }
    }

    pub fn new_null() -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::default()),
            clock: SteadyClock::new_null(),
            #[cfg(feature = "rai_protocol")]
            recovery_candidates: Default::default(),
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
            // `Election::is_final()` means Final-vote generation is eligible, not that the
            // election has actually finalized. Every still-active election belongs in the
            // closing snapshot.
            .filter(|election| election.qualified_root().epoch == epoch)
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
        status.finalized = slots
            .iter()
            .filter(|slot| finalized.contains_key(slot))
            .count();
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

    #[cfg(feature = "rai_protocol")]
    pub fn stalled_cut_details(
        &self,
        epoch: u64,
        slots: &std::collections::HashSet<rsnano_types::SlotRoot>,
    ) -> Vec<String> {
        self.aec
            .read()
            .unwrap()
            .iter_round_robin()
            .filter(|election| {
                election.qualified_root().epoch == epoch
                    && slots.contains(&election.qualified_root().slot())
                    && !election.is_terminated()
                    && !election.is_terminated()
            })
            .map(|election| election.rai_debug_status())
            .collect()
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
            if let Some(election) = guard.election_for_root(&slot.with_epoch(epoch)) {
                if !election.is_terminated() {
                    return None;
                }
                // A finalized value is authoritative. Before finalization, selection advances
                // monotonically to the greatest hash with a notarization certificate. A
                // timeout-only termination contributes no value.
                if let Some(hash) = finalized.get(slot) {
                    values.insert(*slot, *hash);
                } else if let Some(hash) = election.notarized_value() {
                    values.insert(*slot, hash);
                }
            } else if let Some(hash) = finalized.get(slot) {
                // Confirmed elections may already have left the active container. Such a slot
                // has a finalized (and therefore notarized) value rather than a timeout result.
                values.insert(*slot, *hash);
            } else {
                return None;
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
    pub fn cut_recovery_targets(
        &self,
        epoch: u64,
        slots: &std::collections::HashSet<rsnano_types::SlotRoot>,
        committee: &std::collections::HashSet<rsnano_types::PublicKey>,
        include_terminated: bool,
    ) -> Vec<(rsnano_types::PublicKey, VoteType, BlockHash, rsnano_types::Root)> {
        let guard = self.aec.read().unwrap();
        let mut targets: Vec<_> = guard
            .iter_round_robin()
            .filter(|election| {
                election.qualified_root().epoch == epoch
                    && slots.contains(&election.qualified_root().slot())
                    && (include_terminated || !election.is_terminated())
            })
            .flat_map(|election| {
                let candidates: Vec<_> = election
                    .candidate_blocks()
                    .values()
                    .map(|block| (block.hash(), block.root()))
                    .collect();
                let votes = election.votes();
                let missing_first: Vec<_> = committee
                    .iter()
                    .filter(|voter| votes.get(voter).is_none_or(|vote| vote.first.is_none()))
                    .copied()
                    .collect();
                if !missing_first.is_empty() {
                    return missing_first
                        .into_iter()
                        .filter_map(|voter| {
                            candidates.first().map(|(_, root)| (voter, VoteType::First, BlockHash::default(), *root))
                        })
                        .collect::<Vec<_>>();
                }
                let missing_second: Vec<_> = election
                    .second_look_targets()
                    .flat_map(|hash| {
                        let root = candidates
                            .iter()
                            .find_map(|(candidate, root)| (*candidate == hash).then_some(*root))
                            .unwrap_or_default();
                        committee.iter().filter_map(move |voter| {
                            votes
                                .get(voter)
                                .is_none_or(|vote| !vote.notarized.contains(&hash))
                                .then_some((*voter, VoteType::NonFinal, hash, root))
                        })
                    })
                    .collect();
                if !missing_second.is_empty() {
                    return missing_second;
                }
                Vec::new()
            })
            .collect();
        targets.sort_unstable_by_key(|(voter, vote_type, hash, root)| {
            let phase = match vote_type {
                VoteType::First => 0,
                VoteType::NonFinal => 1,
                VoteType::Timeout => 2,
                VoteType::Final => 3,
            };
            (*voter, phase, *hash, *root)
        });
        targets.dedup();
        targets
    }

    #[cfg(feature = "rai_protocol")]
    pub fn cut_repair_blocks(
        &self,
        epoch: u64,
        slots: &std::collections::HashSet<rsnano_types::SlotRoot>,
    ) -> Vec<rsnano_types::Block> {
        let guard = self.aec.read().unwrap();
        let mut targets: Vec<_> = guard
            .iter_round_robin()
            .filter(|election| {
                election.qualified_root().epoch == epoch
                    && slots.contains(&election.qualified_root().slot())
            })
            .flat_map(|election| {
                election
                    .candidate_blocks()
                    .values()
                    .cloned()
                    .map(rsnano_types::Block::from)
            })
            .collect();
        targets.sort_unstable_by_key(|block| block.hash());
        targets.dedup_by_key(|block| block.hash());
        targets
    }

    #[cfg(feature = "rai_protocol")]
    pub fn candidate_blocks_for_epoch(&self, epoch: u64) -> Vec<rsnano_types::Block> {
        let guard = self.aec.read().unwrap();
        let mut blocks: Vec<_> = guard
            .iter_round_robin()
            .filter(|election| election.qualified_root().epoch == epoch)
            .flat_map(|election| {
                election
                    .candidate_blocks()
                    .values()
                    .cloned()
                    .map(rsnano_types::Block::from)
            })
            .collect();
        blocks.sort_unstable_by_key(|block| block.hash());
        blocks.dedup_by_key(|block| block.hash());
        blocks
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
