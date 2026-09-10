use std::{
    collections::VecDeque,
    mem::size_of,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rsnano_ledger::{AnySet, Ledger};
use rsnano_messages::{ConfirmAck, Message};
use rsnano_network::{Channel, ChannelId, TrafficType};
use rsnano_nullable_clock::SteadyClock;
#[cfg(not(feature = "rai_protocol"))]
use rsnano_types::UnixMillisTimestamp;
use rsnano_types::{BlockHash, Root, SavedBlock, Vote};
use rsnano_utils::{
    container_info::ContainerInfo,
    stats::{DetailType, Direction, Sample, StatType, Stats},
};

use super::{LocalVoteHistory, VoteSpacing};
use crate::{
    consensus::VoteBroadcaster, transport::MessageSender, utils::ProcessingQueue,
    wallets::WalletRepresentatives,
};

/// Vote requested by a given channel
pub struct VoteRequest {
    pub epoch: u64,
    pub candidates: Vec<(Root, BlockHash)>,
    pub channel: Arc<Channel>,
}

pub(crate) struct VoteGenerator {
    ledger: Arc<Ledger>,
    vote_generation_queue: ProcessingQueue<(Root, BlockHash, u64)>,
    shared_state: Arc<SharedState>,
    thread: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<Stats>,
}

impl VoteGenerator {
    const MAX_REQUESTS: usize = 2048;
    const MAX_HASHES: usize = 255;

    pub(crate) fn new(
        ledger: Arc<Ledger>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        history: Arc<LocalVoteHistory>,
        is_final: bool,
        stats: Arc<Stats>,
        message_sender: MessageSender,
        voting_delay: Duration,
        vote_generator_delay: Duration,
        vote_broadcaster: Arc<VoteBroadcaster>,
        clock: Arc<SteadyClock>,
        #[cfg(feature = "rai_protocol")] elections: Arc<
            std::sync::RwLock<std::sync::Weak<crate::consensus::AecService>>,
        >,
        #[cfg(feature = "rai_protocol")] vote_state: Arc<
            Mutex<super::kudzu_vote_state::KudzuVoteState>,
        >,
    ) -> Self {
        let shared_state = Arc::new(SharedState {
            #[cfg(feature = "rai_protocol")]
            elections,
            #[cfg(feature = "rai_protocol")]
            vote_state,
            ledger: Arc::clone(&ledger),
            #[cfg(feature = "rai_protocol")]
            pending: Mutex::new(Default::default()),
            message_sender: Mutex::new(message_sender),
            history,
            wallet_reps,
            condition: Condvar::new(),
            queues: Mutex::new(Queues {
                requests: Default::default(),
                candidates: Default::default(),
                next_broadcast: Instant::now(),
            }),
            is_final,
            stopped: AtomicBool::new(false),
            stats: Arc::clone(&stats),
            vote_broadcaster,
            spacing: Mutex::new(VoteSpacing::new(voting_delay)),
            vote_generator_delay,
            clock,
        });

        let shared_state_clone = Arc::clone(&shared_state);
        Self {
            ledger,
            shared_state,
            thread: Mutex::new(None),
            vote_generation_queue: ProcessingQueue::new(
                Arc::clone(&stats),
                shared_state_clone.stat_type(),
                Self::thread_name(is_final),
                1,         // single threaded
                1024 * 32, // max queue size
                256,       // max batch size,
                Box::new(move |batch| {
                    shared_state_clone.process_batch(batch);
                }),
            ),
            stats,
        }
    }

    fn thread_name(is_final: bool) -> String {
        if is_final {
            "Voting final".to_owned()
        } else {
            "Voting".to_owned()
        }
    }

    pub(crate) fn start(&self) {
        let shared_state_clone = Arc::clone(&self.shared_state);
        *self.thread.lock().unwrap() = Some(
            thread::Builder::new()
                .name(Self::thread_name(self.shared_state.is_final))
                .spawn(move || shared_state_clone.run())
                .unwrap(),
        );
        self.vote_generation_queue.start();
    }

    pub(crate) fn stop(&self) {
        self.vote_generation_queue.stop();
        {
            let _guard = self.shared_state.queues.lock().unwrap();
            self.shared_state.stopped.store(true, Ordering::SeqCst);
        }
        self.shared_state.condition.notify_all();
        let thread = self.thread.lock().unwrap().take();
        if let Some(thread) = thread {
            thread.join().unwrap();
        }
    }

    /// Queue items for vote generation, or broadcast votes already in cache
    pub(crate) fn add_in_epoch(&self, root: &Root, hash: &BlockHash, epoch: u64) {
        let candidate = (*root, *hash, epoch);
        #[cfg(feature = "rai_protocol")]
        {
            // Coalesce retries while the same statement is already queued.
            // Remove the reservation on rejection so a later retry remains possible.
            if !self.shared_state.pending.lock().unwrap().insert(candidate) {
                return;
            }
            if !self.vote_generation_queue.try_add(candidate) {
                self.shared_state.pending.lock().unwrap().remove(&candidate);
            }
        }
        #[cfg(not(feature = "rai_protocol"))]
        self.vote_generation_queue.add(candidate);
    }

    /// Queue blocks for vote generation, returning the number of successful candidates.
    pub(crate) fn generate_in_epoch(
        &self,
        blocks: &[SavedBlock],
        channel: &Arc<Channel>,
        epoch: u64,
    ) -> usize {
        let req_candidates = {
            let any = self.ledger.any();

            let can_vote = |block: &SavedBlock| {
                #[cfg(feature = "ledger_snapshots")]
                {
                    // With ledger snapshots enabled, we just stop voting for forks, because
                    // fork rollback will happen when a new snapshot is created
                    any.dependencies_confirmed(block)
                        && (!any.is_forked(&block.qualified_root()) || {
                            // For now allow final votes, until we include final voted fronties in
                            // the preproposals!
                            self.shared_state.is_final
                        })
                }
                #[cfg(not(feature = "ledger_snapshots"))]
                {
                    any.dependencies_confirmed(block)
                }
            };

            blocks
                .iter()
                .filter_map(|i| {
                    if can_vote(i) {
                        Some((i.root(), i.hash()))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };

        let req_candidates: Vec<_> = self
            .ledger
            .verify_votes(
                req_candidates.into(),
                self.shared_state.is_final && !cfg!(feature = "rai_protocol"),
            )
            .into();
        let result = req_candidates.len();
        let mut guard = self.shared_state.queues.lock().unwrap();
        let vote_req = VoteRequest {
            epoch,
            candidates: req_candidates,
            channel: channel.clone(),
        };
        guard.requests.push_back(vote_req);
        while guard.requests.len() > Self::MAX_REQUESTS {
            // On a large queue of requests, erase the oldest one
            guard.requests.pop_front();
            self.stats.inc(
                self.shared_state.stat_type(),
                DetailType::GeneratorRepliesDiscarded,
            );
        }

        result
    }

    pub(crate) fn container_info(&self) -> ContainerInfo {
        let candidates_count;
        let requests_count;
        {
            let guard = self.shared_state.queues.lock().unwrap();
            candidates_count = guard.candidates.len();
            requests_count = guard.requests.len();
        }

        [
            (
                "candidates",
                candidates_count,
                size_of::<Root>() + size_of::<BlockHash>(),
            ),
            (
                "requests",
                requests_count,
                size_of::<ChannelId>() + size_of::<Vec<(Root, BlockHash)>>(),
            ),
        ]
        .into()
    }
}

impl Drop for VoteGenerator {
    fn drop(&mut self) {
        debug_assert!(self.thread.lock().unwrap().is_none())
    }
}

struct SharedState {
    #[cfg(feature = "rai_protocol")]
    pending: Mutex<std::collections::HashSet<(Root, BlockHash, u64)>>,
    #[cfg(feature = "rai_protocol")]
    vote_state: Arc<Mutex<super::kudzu_vote_state::KudzuVoteState>>,
    #[cfg(feature = "rai_protocol")]
    elections: Arc<std::sync::RwLock<std::sync::Weak<crate::consensus::AecService>>>,
    ledger: Arc<Ledger>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    history: Arc<LocalVoteHistory>,
    message_sender: Mutex<MessageSender>,
    is_final: bool,
    condition: Condvar,
    stopped: AtomicBool,
    queues: Mutex<Queues>,
    stats: Arc<Stats>,
    vote_broadcaster: Arc<VoteBroadcaster>,
    spacing: Mutex<VoteSpacing>,
    vote_generator_delay: Duration,
    clock: Arc<SteadyClock>,
}

impl SharedState {
    fn run(&self) {
        let mut queues = self.queues.lock().unwrap();
        while !self.stopped.load(Ordering::SeqCst) {
            queues = self
                .condition
                .wait_timeout_while(queues, self.vote_generator_delay, |i| {
                    !self.stopped.load(Ordering::SeqCst)
                        && i.requests.is_empty()
                        && !i.should_broadcast()
                })
                .unwrap()
                .0;

            if self.stopped.load(Ordering::SeqCst) {
                return;
            }

            if queues.should_broadcast() {
                queues = self.broadcast(queues);
                queues.next_broadcast = Instant::now() + self.vote_generator_delay;
            }

            if let Some(request) = queues.requests.pop_front() {
                drop(queues);
                self.reply(request);
                queues = self.queues.lock().unwrap();
            }
        }
    }

    fn broadcast<'a>(&'a self, mut queues: MutexGuard<'a, Queues>) -> MutexGuard<'a, Queues> {
        #[cfg(feature = "rai_protocol")]
        {
            // Each epoch has a separate signed batch, but ready epochs share this
            // wakeup. Recovery for a closed epoch must not impose another full
            // batching delay on fresh work. Snapshot the set to bound this pass.
            let epochs: std::collections::BTreeSet<_> =
                queues.candidates.iter().map(|c| c.2).collect();
            for epoch in epochs {
                queues = self.broadcast_epoch(queues, epoch);
            }
            queues
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            let epoch = queues.candidates.front().map(|c| c.2).unwrap_or(0);
            self.broadcast_epoch(queues, epoch)
        }
    }

    fn broadcast_epoch<'a>(
        &'a self,
        mut queues: MutexGuard<'a, Queues>,
        epoch: u64,
    ) -> MutexGuard<'a, Queues> {
        let mut hashes = Vec::with_capacity(VoteGenerator::MAX_HASHES);
        let mut roots = Vec::with_capacity(VoteGenerator::MAX_HASHES);
        {
            let spacing = self.spacing.lock().unwrap();
            // Inspect each queued item at most once. Interleaved epochs must not
            // turn a full batch into single-hash packets at an epoch boundary.
            let queued = queues.candidates.len();
            for _ in 0..queued {
                let Some((root, hash, candidate_epoch)) = queues.candidates.pop_front() else {
                    break;
                };
                if candidate_epoch != epoch {
                    #[cfg(feature = "rai_protocol")]
                    {
                        queues.candidates.push_back((root, hash, candidate_epoch));
                        continue;
                    }
                    #[cfg(not(feature = "rai_protocol"))]
                    {
                        queues.candidates.push_front((root, hash, candidate_epoch));
                        break;
                    }
                }
                #[cfg(feature = "rai_protocol")]
                if roots.contains(&root) {
                    // Different values for one root need separate signed batches.
                    // Retain the candidate and its pending reservation for the next batch.
                    queues.candidates.push_back((root, hash, candidate_epoch));
                    continue;
                }
                #[cfg(feature = "rai_protocol")]
                self.pending
                    .lock()
                    .unwrap()
                    .remove(&(root, hash, candidate_epoch));
                if !roots.contains(&root) {
                    if spacing.votable_in_epoch(&root, &hash, self.clock.now(), epoch) {
                        roots.push(root);
                        hashes.push(hash);
                    } else {
                        self.stats
                            .inc(self.stat_type(), DetailType::GeneratorSpacing);
                    }
                }
                if hashes.len() == VoteGenerator::MAX_HASHES {
                    break;
                }
            }
        }

        if !hashes.is_empty() {
            drop(queues);
            self.vote(&hashes, &roots, epoch, |generated_vote| {
                self.stats
                    .inc(self.stat_type(), DetailType::GeneratorBroadcasts);
                let sample = if self.is_final {
                    Sample::VoteGeneratorFinalHashes
                } else {
                    Sample::VoteGeneratorHashes
                };
                self.stats.sample(
                    sample,
                    generated_vote.hashes.len() as i64,
                    (0, ConfirmAck::HASHES_MAX as i64),
                );
                self.vote_broadcaster.broadcast(generated_vote);
            });
            queues = self.queues.lock().unwrap();
        }

        queues
    }

    fn vote<F>(&self, hashes: &[BlockHash], roots: &[Root], epoch: u64, action: F)
    where
        F: Fn(Arc<Vote>),
    {
        debug_assert_eq!(hashes.len(), roots.len());
        let mut rep_keys = Vec::new();

        self.wallet_reps
            .lock()
            .unwrap()
            .rep_priv_keys(&mut rep_keys);

        #[cfg(feature = "rai_protocol")]
        {
            self.kudzu_vote(hashes, roots, epoch, rep_keys, action);
            return;
        }
        #[cfg(not(feature = "rai_protocol"))]
        let mut votes = Vec::new();
        #[cfg(not(feature = "rai_protocol"))]
        for rep_key in rep_keys.drain(..) {
            let timestamp = if self.is_final {
                Vote::TIMESTAMP_MAX
            } else {
                UnixMillisTimestamp::now()
            };
            let duration = if self.is_final {
                Vote::DURATION_MAX
            } else {
                0x9 /*8192ms*/
            };
            votes.push(Arc::new(Vote::new_in_epoch(
                &rep_key,
                timestamp,
                duration,
                hashes.to_vec(),
                epoch,
            )));
        }

        #[cfg(not(feature = "rai_protocol"))]
        for vote in votes {
            {
                let now = self.clock.now();
                let mut spacing = self.spacing.lock().unwrap();
                for i in 0..hashes.len() {
                    self.history.add(&roots[i], &hashes[i], &vote);
                    spacing.flag_in_epoch(&roots[i], &hashes[i], now, epoch);
                }
            }
            action(vote);
        }
    }

    /// Resolve ledger blocks first, then validated candidates retained by the AEC.
    /// Forks need not replace the ledger winner before they can be notarized.
    #[cfg(feature = "rai_protocol")]
    fn kudzu_candidates(
        &self,
        hashes: &[BlockHash],
        roots: &[Root],
    ) -> Vec<(BlockHash, Root, rsnano_types::QualifiedRoot)> {
        let mut blocks: Vec<Option<rsnano_types::Block>> = {
            let any = self.ledger.any();
            hashes
                .iter()
                .map(|h| any.get_block(h).map(Into::into))
                .collect()
        };
        let missing: Vec<_> = blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.is_none().then_some(i))
            .collect();
        if !missing.is_empty() {
            if let Some(aec) = self.elections.read().unwrap().upgrade() {
                let candidates =
                    aec.kudzu_candidates(&missing.iter().map(|i| hashes[*i]).collect::<Vec<_>>());
                for (i, block) in missing.into_iter().zip(candidates) {
                    blocks[i] = block;
                }
            }
        }
        let any = self.ledger.any();
        blocks
            .into_iter()
            .zip(hashes)
            .zip(roots)
            .filter_map(|((block, hash), root)| {
                let block = block?;
                (block.hash() == *hash
                    && block.root() == *root
                    && any.dependencies_confirmed_for_unsaved_block(&block))
                .then(|| (*hash, *root, block.qualified_root()))
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    fn kudzu_vote<F>(
        &self,
        hashes: &[BlockHash],
        roots: &[Root],
        epoch: u64,
        rep_keys: Vec<rsnano_types::PrivateKey>,
        action: F,
    ) where
        F: Fn(Arc<Vote>),
    {
        use rsnano_types::{ElectionId, VoteKind};
        // Never hold the election lock while entering a database write transaction.
        let aec = self.elections.read().unwrap().upgrade();
        let mut candidates: Vec<_> = self
            .kudzu_candidates(hashes, roots)
            .into_iter()
            .map(|(hash, root, qualified)| (hash, root, qualified, false, false, false))
            .collect();
        if let Some(aec) = aec {
            let eligibility = aec.kudzu_eligibilities(
                candidates
                    .iter()
                    .map(|(hash, _, root, _, _, _)| (ElectionId::new(root.clone(), epoch), *hash)),
            );
            for (candidate, (second, notar, timeout)) in candidates.iter_mut().zip(eligibility) {
                candidate.3 = second;
                candidate.4 = notar;
                candidate.5 = timeout;
            }
        }
        for key in rep_keys {
            let mut groups =
                std::collections::BTreeMap::<VoteKind, (Vec<BlockHash>, Vec<Root>)>::new();
            let mut authorized = Vec::new();
            {
                // Serialize signing decisions across normal/final/request generators.
                let mut state = self.vote_state.lock().unwrap();
                let draining = self
                    .ledger
                    .draining_epoch
                    .load(std::sync::atomic::Ordering::Acquire);
                if draining != u64::MAX {
                    state.drain_through(draining);
                }
                if self.is_final {
                    let tx = self.ledger.store.begin_read();
                    for (hash, root, qualified, second, notar, timeout) in &candidates {
                        let lock = self.ledger.store.final_vote.get(&tx, qualified);
                        if let Some(kind) = state.authorize_timeout(
                            qualified,
                            key.public_key(),
                            *hash,
                            epoch,
                            *timeout,
                            lock,
                        ) {
                            let group = groups.entry(kind).or_default();
                            group.0.push(*hash);
                            group.1.push(*root);
                        }
                        if *second {
                            if let Some(first) = state.first_value(qualified, key.public_key()) {
                                if first != *hash
                                    && !state.has_first(qualified, key.public_key(), first, epoch)
                                {
                                    if let Some(kind) = state.authorize(
                                        qualified,
                                        key.public_key(),
                                        first,
                                        epoch,
                                        false,
                                        false,
                                        false,
                                        lock,
                                    ) {
                                        let group = groups.entry(kind).or_default();
                                        group.0.push(first);
                                        group.1.push(*root);
                                    }
                                }
                            }
                        }
                        // Preserve First recovery after election removal, but do
                        // not duplicate an active election's already-issued First.
                        let needs_first =
                            !*notar || !state.has_first(qualified, key.public_key(), *hash, epoch);
                        if needs_first {
                            if let Some(kind) = state.authorize(
                                qualified,
                                key.public_key(),
                                *hash,
                                epoch,
                                false,
                                *second,
                                *notar,
                                lock,
                            ) {
                                let group = groups.entry(kind).or_default();
                                group.0.push(*hash);
                                group.1.push(*root);
                            }
                        }
                        if let Some(kind) = state.authorize(
                            qualified,
                            key.public_key(),
                            *hash,
                            epoch,
                            true,
                            *second,
                            *notar,
                            lock,
                        ) {
                            if lock == Some(*hash) {
                                let group = groups.entry(kind).or_default();
                                group.0.push(*hash);
                                group.1.push(*root);
                            } else {
                                authorized.push((*hash, *root, qualified, kind));
                            }
                        }
                    }
                    drop(tx);
                } else {
                    let tx = self.ledger.store.begin_read();
                    for (hash, root, qualified, second, notar, timeout) in &candidates {
                        let lock = self.ledger.store.final_vote.get(&tx, qualified);
                        if let Some(kind) = state.authorize_timeout(
                            qualified,
                            key.public_key(),
                            *hash,
                            epoch,
                            *timeout,
                            lock,
                        ) {
                            let group = groups.entry(kind).or_default();
                            group.0.push(*hash);
                            group.1.push(*root);
                        }
                        if *second {
                            if let Some(first) = state.first_value(qualified, key.public_key()) {
                                if first != *hash
                                    && !state.has_first(qualified, key.public_key(), first, epoch)
                                {
                                    if let Some(kind) = state.authorize(
                                        qualified,
                                        key.public_key(),
                                        first,
                                        epoch,
                                        false,
                                        false,
                                        false,
                                        lock,
                                    ) {
                                        let group = groups.entry(kind).or_default();
                                        group.0.push(first);
                                        group.1.push(*root);
                                    }
                                }
                            }
                        }
                        if let Some(kind) = state.authorize(
                            qualified,
                            key.public_key(),
                            *hash,
                            epoch,
                            false,
                            *second,
                            *notar,
                            lock,
                        ) {
                            let group = groups.entry(kind).or_default();
                            group.0.push(*hash);
                            group.1.push(*root);
                        }
                    }
                }
            }
            let emit = |kind, hashes: Vec<BlockHash>, roots: Vec<Root>| {
                for (hashes, roots) in hashes
                    .chunks(Vote::MAX_HASHES)
                    .zip(roots.chunks(Vote::MAX_HASHES))
                {
                    let vote = Arc::new(Vote::new_with_kind(&key, hashes.to_vec(), epoch, kind));
                    {
                        let mut spacing = self.spacing.lock().unwrap();
                        for (hash, root) in hashes.iter().zip(roots) {
                            self.history.add(root, hash, &vote);
                            spacing.flag_in_epoch(root, hash, self.clock.now(), epoch);
                        }
                    }
                    crate::consensus::epoch_closer::debug_trace(
                        || serde_json::json!({"type":"vote_generated","epoch":epoch,"voter":vote.voter,"kind":format!("{:?}",kind),"hashes":hashes}),
                    );
                    action(vote);
                }
            };
            // First/notarization recovery is independent of final-lock I/O.
            for kind in [
                VoteKind::FirstTimeout,
                VoteKind::First,
                VoteKind::Notarize,
                VoteKind::Timeout,
            ] {
                if let Some((hashes, roots)) = groups.remove(&kind) {
                    emit(kind, hashes, roots);
                }
            }
            // The in-memory final reservation prevents conflicting signatures
            // while the legacy final lock is persisted. Never block First signing
            // for unrelated roots behind the ledger's writer lock.
            if !authorized.is_empty() {
                let mut tx = self.ledger.store.begin_write();
                for (hash, root, qualified, kind) in authorized {
                    assert!(self.ledger.store.final_vote.put(&mut tx, qualified, &hash));
                    let group = groups.entry(kind).or_default();
                    group.0.push(hash);
                    group.1.push(root);
                }
                tx.commit();
            }
            for (kind, (hashes, roots)) in groups {
                emit(kind, hashes, roots);
            }
        }
    }

    fn reply(&self, request: VoteRequest) {
        let mut i = request.candidates.iter().peekable();
        while i.peek().is_some() && !self.stopped.load(Ordering::SeqCst) {
            let mut hashes = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            let mut roots = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            {
                let spacing = self.spacing.lock().unwrap();
                while hashes.len() < VoteGenerator::MAX_HASHES {
                    let Some((root, hash)) = i.next() else {
                        break;
                    };
                    if !roots.contains(root) {
                        if spacing.votable_in_epoch(root, hash, self.clock.now(), request.epoch) {
                            roots.push(*root);
                            hashes.push(*hash);
                        } else {
                            self.stats
                                .inc(self.stat_type(), DetailType::GeneratorSpacing);
                        }
                    }
                }
            }
            if !hashes.is_empty() {
                self.stats.add_dir(
                    StatType::Requests,
                    DetailType::RequestsGeneratedHashes,
                    Direction::In,
                    hashes.len() as u64,
                );
                self.vote(&hashes, &roots, request.epoch, |vote| {
                    #[cfg(feature = "rai_protocol")]
                    if !self.vote_broadcaster.enqueue_local(vote.clone()) {
                        return;
                    }
                    let confirm =
                        Message::ConfirmAck(ConfirmAck::new_with_own_vote((*vote).clone()));
                    self.message_sender.lock().unwrap().try_send(
                        &request.channel,
                        &confirm,
                        TrafficType::Vote,
                    );
                    self.stats.inc_dir(
                        StatType::Requests,
                        DetailType::RequestsGeneratedVotes,
                        Direction::In,
                    );
                });
            }
        }
        self.stats
            .inc(self.stat_type(), DetailType::GeneratorReplies);
    }

    fn process_batch(&self, batch: VecDeque<(Root, BlockHash, u64)>) {
        #[cfg(feature = "rai_protocol")]
        let submitted: Vec<_> = batch.iter().copied().collect();
        let mut grouped = std::collections::BTreeMap::<u64, VecDeque<(Root, BlockHash)>>::new();
        for (root, hash, epoch) in batch {
            grouped.entry(epoch).or_default().push_back((root, hash));
        }
        let mut verified = VecDeque::new();
        for (epoch, candidates) in grouped {
            #[cfg(feature = "rai_protocol")]
            {
                let (roots, hashes): (Vec<_>, Vec<_>) = candidates.into_iter().unzip();
                verified.extend(
                    self.kudzu_candidates(&hashes, &roots)
                        .into_iter()
                        .map(|(h, r, _)| (r, h, epoch)),
                );
            }
            #[cfg(not(feature = "rai_protocol"))]
            verified.extend(
                self.ledger
                    .verify_votes(candidates, self.is_final && !cfg!(feature = "rai_protocol"))
                    .into_iter()
                    .map(|(r, h)| (r, h, epoch)),
            );
        }

        #[cfg(feature = "rai_protocol")]
        {
            let accepted: std::collections::HashSet<_> = verified.iter().copied().collect();
            let mut pending = self.pending.lock().unwrap();
            for candidate in submitted {
                if !accepted.contains(&candidate) {
                    pending.remove(&candidate);
                }
            }
        }
        // Submit verified candidates to the main processing thread
        if !verified.is_empty() {
            let should_notify = {
                let mut queues = self.queues.lock().unwrap();
                queues.candidates.extend(verified);
                queues.candidates.len() >= VoteGenerator::MAX_HASHES
            };

            if should_notify {
                self.condition.notify_all();
            }
        }
    }

    fn stat_type(&self) -> StatType {
        if self.is_final {
            StatType::VoteGeneratorFinal
        } else {
            StatType::VoteGenerator
        }
    }
}

struct Queues {
    candidates: VecDeque<(Root, BlockHash, u64)>,
    requests: VecDeque<VoteRequest>,
    next_broadcast: Instant,
}

impl Queues {
    fn should_broadcast(&self) -> bool {
        if self.candidates.len() >= ConfirmAck::HASHES_MAX {
            return true;
        }

        !self.candidates.is_empty() && Instant::now() >= self.next_broadcast
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod fork_recovery_tests {
    use super::*;
    use crate::consensus::{
        AecInsertRequest, AecService, ApplyVoteArgs, FilteredVote, ReceivedVote,
    };
    use crate::representatives::QuorumSnapshot;
    use rsnano_ledger::{RepWeights, test_helpers::UnsavedBlockLatticeBuilder};
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{Amount, Block, NetworkType, PrivateKey, VoteDelivery, VoteKind};

    fn generator(ledger: Arc<Ledger>, aec: &Arc<AecService>) -> VoteGenerator {
        VoteGenerator::new(
            ledger,
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            Arc::new(LocalVoteHistory::new(NetworkType::NanoDevNetwork)),
            false,
            Arc::new(Stats::default()),
            MessageSender::new_null(),
            Duration::ZERO,
            Duration::ZERO,
            Arc::new(VoteBroadcaster::new_null()),
            Arc::new(SteadyClock::new_null()),
            Arc::new(std::sync::RwLock::new(Arc::downgrade(aec))),
            Default::default(),
        )
    }

    #[test]
    fn second_look_votes_for_unsaved_fork_and_recovers_first_in_requested_epoch() {
        let ledger = Arc::new(Ledger::new_null());
        let a: Block = UnsavedBlockLatticeBuilder::new().genesis().send(100, 1);
        let b: Block = UnsavedBlockLatticeBuilder::new().genesis().send(200, 1);
        let saved = ledger.process_one(&a).unwrap();
        assert!(ledger.any().get_block(&b.hash()).is_none());
        let aec = Arc::new(AecService::new_null());
        aec.insert(
            AecInsertRequest::new_manual(saved, Default::default()),
            Timestamp::new_test_instance(),
        )
        .unwrap();
        assert!(aec.try_add_fork(&b, Amount::ZERO));
        let mut weights = RepWeights::default();
        for i in 1..=6 {
            weights.put(PrivateKey::from(i).public_key(), Amount::raw(100));
        }
        let mut quorum = QuorumSnapshot::new_test_instance();
        quorum.online_weight = Amount::raw(600);
        quorum.trended_or_min_weight = Amount::raw(600);
        for i in 1..=6 {
            let hash = if i <= 3 { a.hash() } else { b.hash() };
            let vote: FilteredVote = ReceivedVote::new(
                Arc::new(Vote::new_with_kind(
                    &PrivateKey::from(i),
                    vec![hash],
                    0,
                    VoteKind::First,
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
        let generator = generator(ledger, &aec);
        let shared = &generator.shared_state;
        let key = PrivateKey::from(1);
        // Simulate recovery for an outstanding epoch after voting in another epoch.
        assert_eq!(
            shared.vote_state.lock().unwrap().authorize(
                &a.qualified_root(),
                key.public_key(),
                a.hash(),
                1,
                false,
                false,
                false,
                None
            ),
            Some(VoteKind::First)
        );
        shared.process_batch([(b.root(), b.hash(), 0)].into());
        assert_eq!(
            shared.queues.lock().unwrap().candidates.pop_front(),
            Some((b.root(), b.hash(), 0))
        );
        let emitted = std::cell::RefCell::new(Vec::new());
        shared.kudzu_vote(&[b.hash()], &[b.root()], 0, vec![key], |v| {
            emitted.borrow_mut().push(v)
        });
        let votes = emitted.into_inner();
        assert_eq!(votes.len(), 2);
        assert_eq!(
            (votes[0].kind(), votes[0].hashes.clone()),
            (VoteKind::FirstTimeout, vec![a.hash()])
        );
        assert_eq!(
            (votes[1].kind(), votes[1].hashes.clone()),
            (VoteKind::Notarize, vec![b.hash()])
        );
    }

    #[test]
    fn ready_epochs_share_a_wakeup_without_sharing_a_batch() {
        let ledger = Arc::new(Ledger::new_null());
        let aec = Arc::new(AecService::new_null());
        let generator = generator(ledger, &aec);
        let shared = &generator.shared_state;
        let old = (Root::from(1), BlockHash::from(2), 0);
        let old_fork = (Root::from(1), BlockHash::from(3), 0);
        let fresh = (Root::from(4), BlockHash::from(5), 1);
        shared
            .pending
            .lock()
            .unwrap()
            .extend([old, old_fork, fresh]);
        let mut queues = shared.queues.lock().unwrap();
        queues.candidates.extend([old, old_fork, fresh]);
        let queues = shared.broadcast(queues);
        assert_eq!(
            queues.candidates,
            VecDeque::from([old_fork]),
            "a ready new epoch must not wait another generator delay behind old-epoch recovery"
        );
        assert_eq!(
            *shared.pending.lock().unwrap(),
            std::collections::HashSet::from([old_fork])
        );
    }

    #[test]
    fn broadcast_retains_second_candidate_for_next_batch() {
        let ledger = Arc::new(Ledger::new_null());
        let aec = Arc::new(AecService::new_null());
        let generator = generator(ledger, &aec);
        let shared = &generator.shared_state;
        let a = (Root::from(1), BlockHash::from(2), 0);
        let b = (Root::from(1), BlockHash::from(3), 0);
        shared.pending.lock().unwrap().extend([a, b]);
        let mut queues = shared.queues.lock().unwrap();
        queues.candidates.extend([a, b]);
        let queues = shared.broadcast(queues);
        assert_eq!(queues.candidates, VecDeque::from([b]));
        assert_eq!(
            *shared.pending.lock().unwrap(),
            std::collections::HashSet::from([b])
        );
        let queues = shared.broadcast(queues);
        assert!(queues.candidates.is_empty());
        assert!(shared.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn recovered_vote_groups_respect_wire_hash_limit() {
        let ledger = Arc::new(Ledger::new_null());
        let block = UnsavedBlockLatticeBuilder::new().genesis().send(100, 1);
        ledger.process_one(&block).unwrap();
        let aec = Arc::new(AecService::new_null());
        let generator = generator(ledger, &aec);
        let emitted = std::cell::RefCell::new(Vec::new());
        generator.shared_state.kudzu_vote(
            &vec![block.hash(); Vote::MAX_HASHES + 1],
            &vec![block.root(); Vote::MAX_HASHES + 1],
            0,
            vec![PrivateKey::from(1)],
            |v| emitted.borrow_mut().push(v),
        );
        let votes = emitted.into_inner();
        assert_eq!(votes.len(), 2);
        assert_eq!(
            votes.iter().map(|v| v.hashes.len()).sum::<usize>(),
            Vote::MAX_HASHES + 1
        );
        assert!(votes.iter().all(|v| v.hashes.len() <= Vote::MAX_HASHES));
    }

    #[test]
    fn candidate_lookup_rejects_unknown_hash_wrong_root_and_unconfirmed_dependencies() {
        let ledger = Arc::new(Ledger::new_null());
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let parent = lattice.genesis().send(100, 1);
        ledger.process_one(&parent).unwrap();
        let child = lattice.genesis().send(200, 1);
        let saved = ledger.process_one(&child).unwrap();
        let aec = Arc::new(AecService::new_null());
        aec.insert(
            AecInsertRequest::new_manual(saved, Default::default()),
            Timestamp::new_test_instance(),
        )
        .unwrap();
        let generator = generator(ledger.clone(), &aec);
        assert!(
            generator
                .shared_state
                .kudzu_candidates(&[child.hash()], &[child.root()])
                .is_empty()
        );
        ledger.confirm(parent.hash());
        assert_eq!(
            generator
                .shared_state
                .kudzu_candidates(&[child.hash()], &[child.root()])
                .len(),
            1
        );
        assert!(
            generator
                .shared_state
                .kudzu_candidates(&[child.hash()], &[Root::ZERO])
                .is_empty()
        );
        assert!(
            generator
                .shared_state
                .kudzu_candidates(&[BlockHash::from(999)], &[child.root()])
                .is_empty()
        );
    }
}
