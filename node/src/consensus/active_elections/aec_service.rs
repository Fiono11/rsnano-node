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
    pub fn block_tree(&self) -> serde_json::Value {
        #[cfg(feature = "rai_protocol")]
        {
            let aec = self.aec.read().unwrap();
            // Compact outcomes, without block payloads or retained vote proofs.
            serde_json::json!(
                aec.block_tree
                    .entries()
                    .map(|e| (
                        if e.block.is_none() {
                            7u8
                        } else if e.finalized {
                            2
                        } else {
                            1
                        },
                        e.root.clone(),
                        if e.block.is_some() {
                            e.hash()
                        } else {
                            BlockHash::ZERO
                        },
                        e.epoch,
                        0u64,
                    ))
                    .collect::<Vec<_>>()
            )
        }
        #[cfg(not(feature = "rai_protocol"))]
        serde_json::Value::Null
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn elections_terminated(
        &self,
        epoch: u64,
        ids: &[rsnano_types::ElectionId],
    ) -> bool {
        let aec = self.aec.read().unwrap();
        let pending = aec.pending_epoch_drain(epoch, ids);
        static LAST_DIAGNOSTIC: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if !pending.is_empty()
            && now > LAST_DIAGNOSTIC.load(std::sync::atomic::Ordering::Relaxed) + 5
        {
            LAST_DIAGNOSTIC.store(now, std::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "EPOCH_DRAIN_WAIT {}",
                serde_json::json!({"pending":pending.len(),"examples":pending.iter().take(8).map(|id| {
                aec.election_for_id(id).map(|e| e.termination_diagnostic()).unwrap_or_else(|| serde_json::json!({"missing_election":true,"root":id.root,"epoch":id.epoch}))
            }).collect::<Vec<_>>()})
            );
        }
        pending.is_empty()
    }
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn close_epoch(
        &self,
        ledger: &rsnano_ledger::Ledger,
        epoch: u64,
        hashes: &[BlockHash],
    ) -> anyhow::Result<usize> {
        // Serialize the assertion, canonical commit, and removal with vote processing.
        let mut aec = self.aec.write().unwrap();
        aec.assert_epoch_close(epoch, hashes);
        ledger.close_epoch(epoch, hashes)?;
        Ok(aec.discard_closed_epoch(epoch, hashes))
    }
    pub fn termination_audit(&self, offset: usize) -> serde_json::Value {
        self.aec.read().unwrap().termination_audit(offset)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn take_notarization_notifications(
        &self,
    ) -> Vec<(rsnano_types::ElectionId, BlockHash)> {
        self.aec.write().unwrap().take_notarization_notifications()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn recovery_entries(
        &self,
        requests: &[(BlockHash, rsnano_types::Root)],
        epoch: u64,
    ) -> Vec<rsnano_types::RaiBlockTreeEntry> {
        self.aec.read().unwrap().recovery_entries(requests, epoch)
    }

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

    pub fn set_epoch_source(&self, ledger: std::sync::Arc<rsnano_ledger::Ledger>) {
        self.aec.write().unwrap().epoch_source = Some(ledger);
    }

    // --- Read forwarding ---

    pub fn check_vacancy<T>(&self, source: &T) -> bool
    where
        T: ElectionCandidateSource,
    {
        self.aec.read().unwrap().check_vacancy(source)
    }

    pub fn election_for_id(&self, id: &rsnano_types::ElectionId) -> Option<Election> {
        self.aec.read().unwrap().election_for_id(id).cloned()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn kudzu_eligibilities(
        &self,
        candidates: impl IntoIterator<Item = (rsnano_types::ElectionId, BlockHash)>,
    ) -> Vec<(bool, bool, bool)> {
        let aec = self.aec.read().unwrap();
        candidates
            .into_iter()
            .map(|(id, hash)| {
                aec.election_for_id(&id)
                    .map(|e| {
                        (
                            e.can_notarize(&hash),
                            e.has_kudzu_certificate(hash, rsnano_types::VoteKind::Notarize),
                            e.should_timeout(),
                        )
                    })
                    .unwrap_or_default()
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn kudzu_candidates(&self, hashes: &[BlockHash]) -> Vec<Option<Block>> {
        let aec = self.aec.read().unwrap();
        hashes
            .iter()
            .map(|hash| {
                aec.election_for_block(hash)?
                    .candidate_blocks()
                    .get(hash)
                    .cloned()
                    .map(Into::into)
            })
            .collect()
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
    pub(crate) fn any_active_hash<'a>(&self, hashes: impl Iterator<Item = &'a BlockHash>) -> bool {
        let aec = self.aec.read().unwrap();
        let mut hashes = hashes;
        hashes.any(|hash| aec.is_active_hash(hash))
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

    pub fn remove_votes_in_epoch<'a>(
        &self,
        root: &QualifiedRoot,
        epoch: u64,
        voters: impl IntoIterator<Item = &'a PublicKey>,
    ) {
        self.aec
            .write()
            .unwrap()
            .remove_votes_in_epoch(root, epoch, voters);
    }
    pub fn erase(&self, root: &QualifiedRoot) -> bool {
        self.aec.write().unwrap().erase(root)
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
    pub epoch: u64,
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
