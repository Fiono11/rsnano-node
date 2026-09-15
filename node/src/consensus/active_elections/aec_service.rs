#[cfg(feature = "rai_protocol")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{sync::RwLock, time::Duration};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{
    Account, Amount, Block, BlockHash, PublicKey, QualifiedRoot, SavedBlock, VoteError,
};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};
use rustc_hash::FxHashMap;

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
    #[cfg(feature = "rai_protocol")]
    terminated_count: AtomicU64,
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
                serde_json::json!({"pid":std::process::id(),"epoch":epoch,"pending":pending.len(),"examples":pending.iter().take(8).map(|id| {
                aec.election_for_id(id).map(|e| e.termination_diagnostic()).unwrap_or_else(|| serde_json::json!({"missing_election":true,"root":id.root,"epoch":id.epoch}))
            }).collect::<Vec<_>>()})
            );
        }
        pending.is_empty()
    }
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn pending_first_recovery(
        &self,
        targets: Vec<(rsnano_types::ElectionId, BlockHash)>,
    ) -> Vec<(BlockHash, rsnano_types::Root)> {
        let aec = self.aec.read().unwrap();
        targets
            .into_iter()
            .filter(|(id, _)| {
                !aec.election_for_id(id)
                    .is_some_and(|e| e.has_quorum() || e.is_confirmed() || e.is_timed_out())
                    && !aec
                        .block_tree
                        .for_root(&id.root)
                        .iter()
                        .any(|e| e.epoch == id.epoch)
            })
            .map(|(id, hash)| (hash, id.root.root))
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn terminated_election_count(&self) -> u64 {
        self.terminated_count.load(Ordering::Acquire)
    }

    /// Keep candidate ingestion stable across D3/D4 validation and close signing.
    /// The action receives whether every election of the epoch has an outcome
    /// and the solicitation targets of those that have none yet.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn with_close_readiness<T>(
        &self,
        epoch: u64,
        ids: &[rsnano_types::ElectionId],
        action: impl FnOnce(bool, Vec<(BlockHash, rsnano_types::Root)>) -> T,
    ) -> T {
        let aec = self.aec.read().unwrap();
        let pending = aec.pending_epoch_drain(epoch, ids);
        let targets = pending
            .iter()
            .filter_map(|id| aec.election_for_id(id))
            .map(|e| (e.winner().hash(), e.winner().root()))
            .collect();
        if !pending.is_empty()
            && aec.epoch_source.as_ref().is_some_and(|l| {
                l.draining_epoch.load(std::sync::atomic::Ordering::Acquire) == epoch
            })
        {
            static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if now > LAST.load(std::sync::atomic::Ordering::Relaxed) + 5 {
                LAST.store(now, std::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    "EPOCH_DRAIN_WAIT {}",
                    serde_json::json!({"pid":std::process::id(),"epoch":epoch,"pending":pending.len(),"examples":pending.iter().take(4).map(|id| {
                    aec.election_for_id(id).map(|e| e.termination_diagnostic()).unwrap_or_else(|| serde_json::json!({"missing_election":true,"root":id.root,"epoch":id.epoch}))
                }).collect::<Vec<_>>()})
                );
            }
        }
        action(pending.is_empty(), targets)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn close_epoch(
        &self,
        ledger: &rsnano_ledger::Ledger,
        epoch: u64,
        hashes: &[BlockHash],
    ) -> anyhow::Result<usize> {
        // Stage the expensive membership writes before excluding vote processing.
        // Nothing is visible until commit, and the assertion still covers every
        // finalization received while those writes were being prepared.
        // Lock order is LMDB writer then AEC: other AEC holders must only read
        // the ledger, never acquire its writer while holding an AEC guard.
        let pending = ledger.prepare_epoch_close(epoch, hashes)?;
        let mut aec = self.aec.write().unwrap();
        aec.assert_epoch_close(epoch, hashes);
        pending.commit();
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
            #[cfg(feature = "rai_protocol")]
            terminated_count: AtomicU64::new(0),
        }
    }

    pub fn new_null() -> Self {
        Self {
            aec: RwLock::new(ActiveElectionsContainer::default()),
            clock: SteadyClock::new_null(),
            #[cfg(feature = "rai_protocol")]
            terminated_count: AtomicU64::new(0),
        }
    }

    pub fn set_epoch_source(&self, ledger: std::sync::Arc<rsnano_ledger::Ledger>) {
        let mut aec = self.aec.write().unwrap();
        // Normal startup attaches an empty AEC. Also support attaching/replacing
        // a ledger later: duplicate certificates no longer repeat this work.
        #[cfg(feature = "rai_protocol")]
        for entry in aec.block_tree.entries() {
            if let Some(block) = &entry.block {
                ledger.record_epoch_block(entry.epoch, block.clone());
            }
        }
        aec.epoch_source = Some(ledger);
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
                    .or_else(|| {
                        aec.certificate_recovery.get(&id).map(|votes| {
                            (
                                votes.second_look(&hash),
                                votes.has_certificate(hash, rsnano_types::VoteKind::Notarize),
                                votes.should_timeout(),
                            )
                        })
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
                aec.election_for_block(hash)
                    .and_then(|e| e.candidate_blocks().get(hash).cloned())
                    .map(Into::into)
                    .or_else(|| {
                        let root = aec.block_tree.root_of(hash)?;
                        aec.block_tree
                            .for_root(root)
                            .into_iter()
                            .find(|entry| entry.hash() == *hash && entry.block.is_some())
                            .and_then(|entry| entry.block)
                    })
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

    /// True when the block already has an election or a recent outcome, i.e. when
    /// scheduling it again could only produce a duplicate or a rejected insert.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn is_active_or_recently_confirmed(&self, block_hash: &BlockHash) -> bool {
        let aec = self.aec.read().unwrap();
        aec.is_active_hash(block_hash) || aec.was_recently_confirmed(block_hash)
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
    ) -> FxHashMap<BlockHash, Result<(), VoteError>> {
        let mut aec = self.aec.write().unwrap();
        let results = aec.apply_vote(args);
        #[cfg(feature = "rai_protocol")]
        // Publish only after certificate membership and its outcome are visible.
        // The distinct election IDs remain counted after epoch retirement.
        self.terminated_count
            .store(aec.terminated_elections.len() as u64, Ordering::Release);
        results
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

#[cfg(test)]
mod local_vote_removal_tests {
    use super::*;
    use crate::consensus::{LocalVoteHistory, LocalVotesRemover};
    use rsnano_types::Vote;
    use std::sync::{Arc, mpsc};

    #[test]
    fn empty_local_vote_removal_does_not_wait_for_the_aec_writer() {
        assert_empty_removal_completes_while_writer_is_held(false);
    }

    #[test]
    fn epoch_filtered_empty_removal_erases_only_target_epoch_history() {
        assert_empty_removal_completes_while_writer_is_held(true);
    }

    fn assert_empty_removal_completes_while_writer_is_held(populate_history: bool) {
        let service = Arc::new(AecService::new_null());
        let history = Arc::new(LocalVoteHistory::with_max_cache(16));
        let root = QualifiedRoot::new_test_instance();
        let previous_winner = BlockHash::from(1);
        let other_hash = BlockHash::from(2);
        if populate_history {
            let current_epoch_vote = Arc::new(Vote::null());
            history.add(&root.root, &other_hash, &current_epoch_vote);
            let mut next_epoch_vote = Vote::null();
            next_epoch_vote.epoch = 1;
            history.add(&root.root, &previous_winner, &Arc::new(next_epoch_vote));
        }
        let remover = LocalVotesRemover {
            vote_history: history.clone(),
            active_elections: service.clone(),
        };
        let writer = service.aec.write().unwrap();
        let result = std::thread::scope(|scope| {
            let (tx, rx) = mpsc::channel();
            let root = &root;
            let worker = scope.spawn(move || {
                remover.remove_local_votes_in_epoch(&previous_winner, root, 0);
                tx.send(()).unwrap();
            });
            let result = rx.recv_timeout(Duration::from_secs(2));
            // Release before joining so a regression fails without hanging.
            drop(writer);
            worker.join().unwrap();
            result
        });
        assert_eq!(result, Ok(()), "empty removal waited for the AEC writer");
        assert!(history.votes(&root.root, &other_hash, false).is_empty());
        let remaining = history.votes(&root.root, &previous_winner, false);
        assert_eq!(remaining.len(), usize::from(populate_history));
        if populate_history {
            assert_eq!(remaining[0].epoch, 1);
        }
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

#[cfg(all(test, feature = "rai_protocol"))]
mod epoch_close_tests {
    use super::*;
    use rsnano_ledger::{LedgerBuilder, LedgerConstants};
    use std::sync::atomic::Ordering;

    #[test]
    fn attaching_or_replacing_ledger_backfills_retained_epoch_membership() {
        let service = AecService::new_null();
        let block = SavedBlock::new_test_instance();
        {
            let mut aec = service.aec.write().unwrap();
            for epoch in [0, 1] {
                aec.block_tree
                    .insert(rsnano_types::RaiBlockTreeEntry::notarized(
                        block.clone().into(),
                        epoch,
                    ))
                    .unwrap();
            }
            let timed_out = SavedBlock::new_test_instance_with_key(2);
            aec.block_tree
                .insert(rsnano_types::RaiBlockTreeEntry::timeout(
                    timed_out.qualified_root(),
                    0,
                    timed_out.hash(),
                ))
                .unwrap();
        }
        for _ in 0..2 {
            let ledger = std::sync::Arc::new(rsnano_ledger::Ledger::new_null());
            service.set_epoch_source(ledger.clone());
            for epoch in [0, 1] {
                assert_eq!(ledger.epoch_close_candidate(epoch), vec![block.hash()]);
            }
        }
    }

    #[test]
    fn finalized_omission_aborts_staged_close_before_persistence() {
        let path = std::env::temp_dir().join(format!("rai-close-assert-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(25).unwrap();
            let service = AecService::new_null();
            let mut entry = rsnano_types::RaiBlockTreeEntry::notarized(
                SavedBlock::new_test_instance().into(),
                0,
            );
            entry.finalized = true;
            service
                .aec
                .write()
                .unwrap()
                .block_tree
                .insert(entry)
                .unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                service.close_epoch(&ledger, 0, &[]).unwrap();
            }));
            assert!(result.is_err());
            assert!(ledger.closed_epochs().is_empty());
            assert_eq!(ledger.closed_epoch_count.load(Ordering::Acquire), 0);
            assert_eq!(
                ledger
                    .store
                    .consensus_epochs
                    .closed_blocks(&ledger.store.begin_read()),
                0
            );
        }
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod termination_counter_tests {
    use super::*;
    use crate::{
        consensus::{FilteredVote, ReceivedVote},
        representatives::QuorumSnapshot,
    };
    use rsnano_ledger::{Ledger, RepWeights};
    use rsnano_types::{PrivateKey, Vote, VoteDelivery, VoteKind};
    use std::sync::{Arc, mpsc};

    fn insert(service: &AecService, block: &SavedBlock, epoch: u64) {
        service
            .aec
            .write()
            .unwrap()
            .insert_in_epoch(
                AecInsertRequest::new_manual(block.clone(), Default::default()),
                Timestamp::new_test_instance(),
                epoch,
            )
            .unwrap();
    }

    fn apply(
        service: &AecService,
        rep: u64,
        block: &SavedBlock,
        epoch: u64,
        kind: VoteKind,
    ) -> Result<(), VoteError> {
        let mut weights = RepWeights::default();
        for rep in 1..=6 {
            weights.put(PrivateKey::from(rep).public_key(), Amount::raw(100));
        }
        let mut quorum = QuorumSnapshot::new_test_instance();
        quorum.online_weight = Amount::raw(600);
        quorum.trended_or_min_weight = Amount::raw(600);
        let vote: FilteredVote = ReceivedVote::new(
            Arc::new(Vote::new_with_kind(
                &PrivateKey::from(rep),
                vec![block.hash()],
                epoch,
                kind,
            )),
            VoteDelivery::Direct,
            None,
        )
        .into();
        service.apply_vote(ApplyVoteArgs {
            vote: &vote,
            rep_weights: &weights,
            quorum_snapshot: &quorum,
            now: Timestamp::new_test_instance(),
        })[&block.hash()]
            .clone()
    }

    #[test]
    fn termination_count_tracks_distinct_roots_and_epochs_through_retirement() {
        let service = AecService::new_null();
        let ledger = Arc::new(Ledger::new_null());
        service.set_epoch_source(ledger.clone());
        let block = SavedBlock::new_test_instance_with_key(1);
        let other = SavedBlock::new_test_instance_with_key(2);
        for epoch in [0, 1] {
            insert(&service, &block, epoch);
        }
        insert(&service, &other, 0);
        assert_eq!(service.terminated_election_count(), 0);
        for epoch in [0, 1] {
            for rep in 1..=3 {
                assert_eq!(apply(&service, rep, &block, epoch, VoteKind::First), Ok(()));
                assert_eq!(service.terminated_election_count(), epoch);
            }
            assert_eq!(apply(&service, 4, &block, epoch, VoteKind::First), Ok(()));
            assert_eq!(service.terminated_election_count(), epoch + 1);
            assert_eq!(ledger.epoch_close_candidate(epoch), vec![block.hash()]);
            assert_eq!(
                apply(&service, 4, &block, epoch, VoteKind::First),
                Err(VoteError::Replay)
            );
            for rep in 1..=4 {
                assert_eq!(apply(&service, rep, &block, epoch, VoteKind::Final), Ok(()));
                assert_eq!(service.terminated_election_count(), epoch + 1);
            }
        }
        for rep in 1..=4 {
            assert_eq!(apply(&service, rep, &other, 0, VoteKind::First), Ok(()));
        }
        assert_eq!(service.terminated_election_count(), 3);
        // The unfinalized second root may be omitted by an authenticated close.
        // Its retirement must not roll back the node-lifetime termination count.
        assert_eq!(service.close_epoch(&ledger, 0, &[block.hash()]).unwrap(), 1);
        assert_eq!(service.terminated_election_count(), 3);
        service.stop();
        assert_eq!(service.terminated_election_count(), 3);
    }

    #[test]
    fn timeout_and_block_certificates_count_the_same_election_once() {
        let service = AecService::new_null();
        let block = SavedBlock::new_test_instance();
        insert(&service, &block, 0);
        for rep in 1..=3 {
            assert_eq!(apply(&service, rep, &block, 0, VoteKind::First), Ok(()));
        }
        for rep in 4..=6 {
            assert_eq!(
                apply(&service, rep, &block, 0, VoteKind::FirstTimeout),
                Ok(())
            );
        }
        assert_eq!(service.terminated_election_count(), 0);
        assert_eq!(apply(&service, 1, &block, 0, VoteKind::Timeout), Ok(()));
        assert_eq!(service.terminated_election_count(), 1);
        assert_eq!(apply(&service, 4, &block, 0, VoteKind::Notarize), Ok(()));
        assert_eq!(service.terminated_election_count(), 1);
        assert_eq!(
            service
                .aec
                .read()
                .unwrap()
                .block_tree
                .for_root(&block.qualified_root())
                .len(),
            2
        );
        assert_eq!(
            apply(&service, 1, &block, 0, VoteKind::Timeout),
            Err(VoteError::Replay)
        );
        assert_eq!(service.terminated_election_count(), 1);
    }

    #[test]
    fn termination_count_does_not_wait_for_the_aec_writer() {
        let service = AecService::new_null();
        let block = SavedBlock::new_test_instance();
        insert(&service, &block, 0);
        for rep in 1..=4 {
            assert_eq!(apply(&service, rep, &block, 0, VoteKind::First), Ok(()));
        }
        let writer = service.aec.write().unwrap();
        let result = std::thread::scope(|scope| {
            let (tx, rx) = mpsc::channel();
            let service = &service;
            let reader = scope.spawn(move || {
                tx.send(service.terminated_election_count()).unwrap();
            });
            let result = rx.recv_timeout(Duration::from_secs(2));
            // Release before joining, so a regression reports failure, not a hang.
            drop(writer);
            reader.join().unwrap();
            result
        });
        assert_eq!(result, Ok(1), "count query waited for the AEC writer");
    }
}
