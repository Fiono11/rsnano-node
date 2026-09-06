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
use rsnano_messages::{ConfirmAck, Message, Publish};
use rsnano_network::{Channel, ChannelId, TrafficType};
use rsnano_nullable_clock::SteadyClock;
#[cfg(not(feature = "rai_protocol"))]
use rsnano_types::UnixMillisTimestamp;
use rsnano_types::{BlockHash, MaybeSavedBlock, QualifiedRoot, Root, Vote};
#[cfg(feature = "rai_protocol")]
use rsnano_types::{Signature, VoteType};
use rsnano_utils::{
    container_info::ContainerInfo,
    stats::{DetailType, Direction, Sample, StatType, Stats},
};

use super::{LocalVoteHistory, VoteSpacing};
#[cfg(feature = "rai_protocol")]
use crate::consensus::epochs::VoteGate;
use crate::{
    consensus::VoteBroadcaster, transport::MessageSender, utils::ProcessingQueue,
    wallets::WalletRepresentatives,
};

/// Vote requested by a given channel
pub struct VoteRequest {
    pub candidates: Vec<VoteCandidate>,
    pub channel: Arc<Channel>,
}

pub(crate) struct VoteGenerator {
    ledger: Arc<Ledger>,
    vote_generation_queue: ProcessingQueue<VoteCandidate>,
    shared_state: Arc<SharedState>,
    thread: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<Stats>,
}

impl VoteGenerator {
    #[cfg(feature = "rai_protocol")]
    pub(super) fn reply_earliest_election(&self, slot: rsnano_types::SlotRoot, channel: &Arc<Channel>) {
        let Some((epoch, finalized)) = self.shared_state.active_elections.earliest_election(slot) else {
            return;
        };
        let mut votes = self.shared_state.history.votes_for_epoch(
            &slot.root, epoch, &finalized.unwrap_or_default(),
            finalized.map(|_| VoteType::Final),
        );
        votes.sort_by_key(|vote| match vote.vote_type() {
            VoteType::First => 0,
            VoteType::NonFinal => 1,
            VoteType::Timeout => 2,
            VoteType::Final => 3,
        });
        let mut sent = std::collections::HashSet::new();
        for vote in votes {
            if sent.insert(vote.signature.clone())
                && self.shared_state.reply_replay_filter.lock().unwrap()
                    .should_send(channel.channel_id(), &vote.signature, Instant::now())
            {
                self.shared_state.vote_broadcaster.reply_recovery(vote, channel.channel_id(), false);
            }
        }
        if let Some(hash) = finalized {
            // Fast finalization need not have produced a local Final vote. Generate
            // it from the recorded finalization, even after the epoch has closed.
            let block = self.shared_state.active_elections.candidate_block(epoch, &hash)
                .or_else(|| self.ledger.any().get_block(&hash).map(MaybeSavedBlock::Saved));
            if let Some(block) = block {
                self.generate(&[block], channel, epoch);
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(super) fn reply_election(&self, root: &Root, epoch: u64, channel: &Arc<Channel>) -> bool {
        let votes = self.shared_state.history.votes_for_epoch(
            root, epoch, &BlockHash::default(), Some(self.shared_state.vote_type),
        );
        let found = !votes.is_empty();
        if std::env::var_os("NANO_RAI_RECOVERY_DIAGNOSTICS").is_some() {
            let evidence = self.shared_state.history.votes_for_epoch(root, epoch, &BlockHash::default(), None);
            tracing::info!(target: "rsnano_node::consensus::epochs::coordinator", %root, epoch, kind = ?self.shared_state.vote_type,
                cached = votes.len(), phases = ?evidence.iter().map(|v| v.vote_type()).collect::<Vec<_>>(),
                "RAI diagnostic election reply");
        }
        for vote in votes {
            if self.shared_state.reply_replay_filter.lock().unwrap()
                .should_send(channel.channel_id(), &vote.signature, Instant::now()) {
                self.shared_state.vote_broadcaster.reply_recovery(vote, channel.channel_id(), false);
            }
        }
        found
    }
    #[cfg(feature = "rai_protocol")]
    pub(super) fn reply_block(&self, block: &MaybeSavedBlock, channel: &Arc<Channel>) {
        self.shared_state.message_sender.lock().unwrap().try_send(
            channel,
            &Message::Publish(Publish::new_recovery(block.clone().into())),
            TrafficType::EpochControl,
        );
    }

    const MAX_REQUESTS: usize = 2048;
    const MAX_HASHES: usize = 255;

    #[cfg(feature = "rai_protocol")]
    pub fn cut_generation(&self) -> u64 {
        self.shared_state.vote_gate.cut_generation()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn voting_allowed(&self, root: &QualifiedRoot) -> bool {
        self.shared_state.vote_gate.allows_vote(root)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn clear_vote_spacing(&self) {
        self.shared_state.spacing.lock().unwrap().clear();
        // A cut changes which epoch-e roots may be voted on. Drop scheduler work accumulated
        // under the old policy so the bounded set of cut-recovery phases is not delayed behind
        // thousands of now-ineligible candidates.
        self.vote_generation_queue.clear();
        self.shared_state.queues.lock().unwrap().candidates.clear();
    }

    #[cfg(feature = "rai_protocol")]
    pub(super) fn reply_cached_votes(
        &self,
        blocks: &[MaybeSavedBlock],
        channel: &Arc<Channel>,
        epoch: u64,
    ) {
        let mut votes = Vec::new();
        for block in blocks {
            for vote in self.shared_state.history.votes_for_epoch(
                &block.root(),
                epoch,
                &if self.shared_state.vote_type == VoteType::First { BlockHash::default() } else { block.hash() },
                Some(self.shared_state.vote_type),
            ) {
                if !votes
                    .iter()
                    .any(|existing: &Arc<Vote>| existing.signature == vote.signature)
                {
                    votes.push(vote);
                }
            }
        }
        for vote in votes {
            if !self
                .shared_state
                .reply_replay_filter
                .lock()
                .unwrap()
                .should_send(channel.channel_id(), &vote.signature, Instant::now())
            {
                self.stats.inc(
                    self.shared_state.stat_type(),
                    DetailType::GeneratorReplaySuppressed,
                );
                continue;
            }
            self.shared_state
                .vote_broadcaster
                .reply_recovery(vote, channel.channel_id(), false);
            self.stats.inc_dir(
                StatType::Requests,
                DetailType::RequestsGeneratedVotes,
                Direction::In,
            );
        }
    }

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
        #[cfg(feature = "rai_protocol")] vote_type: VoteType,
        #[cfg(feature = "rai_protocol")] vote_gate: Arc<VoteGate>,
        #[cfg(feature = "rai_protocol")] active_elections: Arc<crate::consensus::AecService>,
    ) -> Self {
        let shared_state = Arc::new(SharedState {
            ledger: Arc::clone(&ledger),
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
            #[cfg(feature = "rai_protocol")]
            vote_type,
            #[cfg(feature = "rai_protocol")]
            reply_replay_filter: Mutex::new(ReplayFilter::new(Duration::from_millis(500))),
            #[cfg(feature = "rai_protocol")]
            vote_gate,
            #[cfg(feature = "rai_protocol")]
            active_elections,
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
    pub(crate) fn add(&self, root: &QualifiedRoot, hash: &BlockHash) {
        self.vote_generation_queue.add(VoteCandidate {
            root: root.root,
            hash: *hash,
            #[cfg(feature = "rai_protocol")]
            epoch: root.epoch,
            #[cfg(feature = "rai_protocol")]
            qualified_root: root.clone(),
        });
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn is_cut_recovery(&self, root: &QualifiedRoot) -> bool {
        self.shared_state.vote_gate.allows_cut_recovery(root)
    }

    /// Queue blocks for vote generation, returning the number of successful candidates.
    pub(crate) fn generate(
        &self,
        blocks: &[MaybeSavedBlock],
        channel: &Arc<Channel>,
        #[cfg(feature = "rai_protocol")] epoch: u64,
    ) -> usize {
        let req_candidates = {
            let any = self.ledger.any();

            let can_vote = |block: &MaybeSavedBlock| {
                #[cfg(feature = "ledger_snapshots")]
                {
                    // With ledger snapshots enabled, we just stop voting for forks, because
                    // fork rollback will happen when a new snapshot is created
                    match block {
                        MaybeSavedBlock::Saved(block) => any.dependencies_confirmed(block),
                        MaybeSavedBlock::Unsaved(block) => {
                            any.dependencies_confirmed_for_unsaved_block(block)
                        }
                    }
                    &&(!any.is_forked(&block.qualified_root()) || {
                        // For now allow final votes, until we include final voted fronties in
                        // the preproposals!
                        self.shared_state.is_final
                    })
                }
                #[cfg(not(feature = "ledger_snapshots"))]
                {
                    let dependencies_confirmed = match block {
                        MaybeSavedBlock::Saved(block) => any.dependencies_confirmed(block),
                        MaybeSavedBlock::Unsaved(block) => {
                            any.dependencies_confirmed_for_unsaved_block(block)
                        }
                    };
                    #[cfg(feature = "rai_protocol")]
                    {
                        dependencies_confirmed
                            || (self.shared_state.is_final
                                && self.shared_state.active_elections.is_finalized_in_epoch(
                                    &block.qualified_root().with_epoch(epoch), &block.hash()))
                            || self
                                .shared_state
                                .vote_gate
                                .allows_cut_recovery(&block.qualified_root().with_epoch(epoch))
                    }
                    #[cfg(not(feature = "rai_protocol"))]
                    dependencies_confirmed
                }
            };

            blocks
                .iter()
                .filter_map(|i| {
                    if can_vote(i) {
                        Some(VoteCandidate {
                            root: i.root(),
                            hash: i.hash(),
                            #[cfg(feature = "rai_protocol")]
                            epoch,
                            #[cfg(feature = "rai_protocol")]
                            qualified_root: i.qualified_root().with_epoch(epoch),
                        })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };

        let result = req_candidates.len();
        let mut guard = self.shared_state.queues.lock().unwrap();
        let vote_req = VoteRequest {
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
                size_of::<ChannelId>() + size_of::<Vec<VoteCandidate>>(),
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
    #[cfg(feature = "rai_protocol")]
    vote_type: VoteType,
    #[cfg(feature = "rai_protocol")]
    reply_replay_filter: Mutex<ReplayFilter>,
    #[cfg(feature = "rai_protocol")]
    vote_gate: Arc<VoteGate>,
    #[cfg(feature = "rai_protocol")]
    active_elections: Arc<crate::consensus::AecService>,
}

#[derive(Clone)]
pub(super) struct VoteCandidate {
    root: Root,
    hash: BlockHash,
    #[cfg(feature = "rai_protocol")]
    epoch: u64,
    #[cfg(feature = "rai_protocol")]
    qualified_root: QualifiedRoot,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VoteOrigin {
    Signed,
    #[cfg(feature = "rai_protocol")]
    Replay,
}

#[cfg(feature = "rai_protocol")]
struct ReplayFilter {
    entries: std::collections::HashMap<(ChannelId, Signature), Instant>,
    interval: Duration,
}

#[cfg(feature = "rai_protocol")]
impl ReplayFilter {
    fn new(interval: Duration) -> Self {
        Self {
            entries: Default::default(),
            interval,
        }
    }

    fn should_send(&mut self, channel: ChannelId, signature: &Signature, now: Instant) -> bool {
        self.entries
            .retain(|_, last_sent| now < *last_sent + self.interval);
        let key = (channel, signature.clone());
        if self.entries.contains_key(&key) {
            false
        } else {
            self.entries.insert(key, now);
            true
        }
    }
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
        let mut hashes = Vec::with_capacity(VoteGenerator::MAX_HASHES);
        let mut roots = Vec::with_capacity(VoteGenerator::MAX_HASHES);
        #[cfg(feature = "rai_protocol")]
        let mut epochs = Vec::with_capacity(VoteGenerator::MAX_HASHES);
        #[cfg(feature = "rai_protocol")]
        let mut qualified_roots = Vec::with_capacity(VoteGenerator::MAX_HASHES);
        #[cfg(feature = "rai_protocol")]
        let mut batch_epoch = None;
        {
            let spacing = self.spacing.lock().unwrap();
            while let Some(candidate) = queues.candidates.pop_front() {
                #[cfg(feature = "rai_protocol")]
                if batch_epoch.is_some_and(|epoch| epoch != candidate.epoch) {
                    queues.candidates.push_front(candidate);
                    break;
                }
                #[cfg(feature = "rai_protocol")]
                {
                    batch_epoch = Some(candidate.epoch);
                }
                let root = candidate.root;
                let hash = candidate.hash;
                let can_add_root = !roots.contains(&root);
                if !can_add_root {
                    queues.candidates.push_front(candidate);
                    break;
                }
                #[cfg(feature = "rai_protocol")]
                let is_votable = self.vote_type == VoteType::NonFinal
                    || spacing.votable(&root, &hash, self.clock.now());
                #[cfg(not(feature = "rai_protocol"))]
                let is_votable = spacing.votable(&root, &hash, self.clock.now());
                if can_add_root {
                    if is_votable {
                        roots.push(root);
                        hashes.push(hash);
                        #[cfg(feature = "rai_protocol")]
                        epochs.push(candidate.epoch);
                        #[cfg(feature = "rai_protocol")]
                        qualified_roots.push(candidate.qualified_root);
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
            // Re-sign exactly the targets selected for this scheduler pass. A cached RAI vote
            // can contain up to 255 hashes; replaying that whole batch because one election was
            // reactivated amplifies recovery traffic and evicts unrelated active elections.
            self.vote(
                &hashes,
                &roots,
                #[cfg(feature = "rai_protocol")]
                &qualified_roots,
                #[cfg(feature = "rai_protocol")]
                &epochs,
                false,
                |generated_vote, _origin| {
                    self.stats
                        .inc(self.stat_type(), DetailType::GeneratorBroadcasts);
                    #[cfg(feature = "rai_protocol")]
                    {
                        self.stats.inc(
                            self.stat_type(),
                            match self.vote_type {
                                VoteType::First => DetailType::GeneratorBroadcastFirst,
                                VoteType::NonFinal => DetailType::GeneratorBroadcastNonFinal,
                                VoteType::Final => DetailType::GeneratorBroadcastFinal,
                                VoteType::Timeout => DetailType::GeneratorBroadcastTimeout,
                            },
                        );
                        self.stats.add(
                            self.stat_type(),
                            match self.vote_type {
                                VoteType::First => DetailType::GeneratorBroadcastFirstHashes,
                                VoteType::NonFinal => DetailType::GeneratorBroadcastNonFinalHashes,
                                VoteType::Final => DetailType::GeneratorBroadcastFinalHashes,
                                VoteType::Timeout => DetailType::GeneratorBroadcastTimeoutHashes,
                            },
                            generated_vote.hashes.len() as u64,
                        );
                    }
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
                },
            );
            queues = self.queues.lock().unwrap();
        }

        queues
    }

    fn vote<F>(
        &self,
        hashes: &[BlockHash],
        roots: &[Root],
        #[cfg(feature = "rai_protocol")] qualified_roots: &[QualifiedRoot],
        #[cfg(feature = "rai_protocol")] epochs: &[u64],
        _replay_cached: bool,
        action: F,
    ) where
        F: Fn(Arc<Vote>, VoteOrigin),
    {
        debug_assert_eq!(hashes.len(), roots.len());
        #[cfg(feature = "rai_protocol")]
        debug_assert_eq!(hashes.len(), epochs.len());
        let mut rep_keys = Vec::new();

        self.wallet_reps
            .lock()
            .unwrap()
            .rep_priv_keys(&mut rep_keys);

        let mut votes = Vec::new();
        #[cfg(feature = "rai_protocol")]
        let mut replay_votes: Vec<Arc<Vote>> = Vec::new();
        for rep_key in rep_keys.drain(..) {
            #[cfg(not(feature = "rai_protocol"))]
            {
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
                votes.push((
                    Arc::new(Vote::new(&rep_key, timestamp, duration, hashes.to_vec())),
                    roots.to_vec(),
                ));
            }
            #[cfg(feature = "rai_protocol")]
            {
                let mut vote_permits = Vec::new();
                let mut pending_hashes = Vec::new();
                let mut pending_roots = Vec::new();
                let mut pending_epochs = Vec::new();
                for (index, ((root, hash), epoch)) in
                    roots.iter().zip(hashes).zip(epochs).enumerate()
                {
                    let existing = self.history.vote_for_epoch(
                        root,
                        *epoch,
                        hash,
                        self.vote_type,
                        rep_key.public_key(),
                    );
                    if let Some(existing) = existing.as_ref()
                        && _replay_cached
                    {
                        if !replay_votes
                            .iter()
                            .any(|vote| vote.signature == existing.signature)
                        {
                            replay_votes.push(existing.clone());
                        }
                        continue;
                    }
                    let qualified_root = qualified_roots[index].clone();
                    let permit = self.vote_gate.enter(&qualified_root);
                    let final_recovery = _replay_cached
                        && self.vote_type == VoteType::Final
                        && (self.vote_gate.allows_final_recovery(&qualified_root, hash)
                            || self.active_elections.is_finalized_in_epoch(&qualified_root, hash));
                    if permit.is_none() && !final_recovery {
                        continue;
                    }
                    vote_permits.extend(permit);
                    // Recheck live First evidence immediately before signing, including
                    // recovery replies. A queued target is not permission to notarize.
                    if self.vote_type == VoteType::NonFinal
                        && (!self.active_elections.second_look_eligible(&qualified_root, hash)
                            || !self.history.has_vote_type(
                                root, *epoch, VoteType::First, rep_key.public_key(),
                            ))
                    {
                        continue;
                    }
                    if self.vote_type == VoteType::First
                        && (pending_roots.contains(root)
                            || self.history.has_vote_type(
                                root,
                                *epoch,
                                VoteType::First,
                                rep_key.public_key(),
                            ))
                    {
                        continue;
                    }
                    if self.vote_type == VoteType::NonFinal
                        && self.history.non_timeout_notarization_count(
                            root,
                            *epoch,
                            rep_key.public_key(),
                        ) >= 3
                    {
                        continue;
                    }
                    if self.vote_type == VoteType::NonFinal
                        && !self
                            .history
                            .can_second_look(root, *epoch, hash, rep_key.public_key())
                    {
                        self.stats.inc(
                            self.stat_type(),
                            DetailType::GeneratorHistorySuppressedNotarized,
                        );
                        continue;
                    }
                    if self.vote_type == VoteType::Final
                        && self.history.has_conflicting_phase_vote(
                            root,
                            *epoch,
                            hash,
                            rep_key.public_key(),
                            final_recovery,
                        )
                    {
                        self.stats.inc(
                            self.stat_type(),
                            DetailType::GeneratorHistorySuppressedConflict,
                        );
                        continue;
                    }
                    {
                        // A confirm_req reply must cover only the requested hashes. Replaying a
                        // cached batched RAI vote here amplifies one requested hash into as many
                        // as 255 unrelated hashes at every receiving node. Signing the requested
                        // subset is safe (the phase and hash are unchanged) and keeps reply work
                        // proportional to the request.
                        pending_roots.push(*root);
                        pending_hashes.push(*hash);
                        pending_epochs.push(*epoch);
                    }
                }
                if !pending_hashes.is_empty() {
                    debug_assert!(
                        pending_epochs
                            .iter()
                            .all(|epoch| *epoch == pending_epochs[0])
                    );
                    votes.push((
                        Arc::new(Vote::new_rai(
                            &rep_key,
                            pending_epochs[0],
                            self.vote_type,
                            pending_hashes,
                        )),
                        pending_roots,
                    ));
                }
                drop(vote_permits);
            }
        }

        #[cfg(feature = "rai_protocol")]
        for vote in replay_votes {
            self.stats
                .inc(self.stat_type(), DetailType::GeneratorReplayVotes);
            self.stats.add(
                self.stat_type(),
                DetailType::GeneratorReplayHashes,
                vote.hashes.len() as u64,
            );
            action(vote, VoteOrigin::Replay);
        }

        for (vote, vote_roots) in votes {
            self.stats
                .inc(self.stat_type(), DetailType::GeneratorSignedVotes);
            self.stats.add(
                self.stat_type(),
                DetailType::GeneratorSignedHashes,
                vote.hashes.len() as u64,
            );
            {
                let now = self.clock.now();
                let mut spacing = self.spacing.lock().unwrap();
                for (root, hash) in vote_roots.iter().zip(&vote.hashes) {
                    self.history.add(root, hash, &vote);
                    spacing.flag(root, hash, now);
                }
            }
            action(vote, VoteOrigin::Signed);
        }
    }

    fn reply(&self, request: VoteRequest) {
        let mut i = request.candidates.iter().peekable();
        while i.peek().is_some() && !self.stopped.load(Ordering::SeqCst) {
            let mut hashes = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            let mut roots = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            #[cfg(feature = "rai_protocol")]
            let mut epochs = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            #[cfg(feature = "rai_protocol")]
            let mut qualified_roots = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            {
                let spacing = self.spacing.lock().unwrap();
                while hashes.len() < VoteGenerator::MAX_HASHES {
                    let Some(candidate) = i.peek() else {
                        break;
                    };
                    let root = &candidate.root;
                    if roots.contains(root) {
                        // Two candidates for one election are distinct phase statements. Keep
                        // them in separate votes so election routing cannot collapse the second
                        // hash into a replay of the first.
                        break;
                    }
                    let candidate = i.next().unwrap();
                    let root = &candidate.root;
                    let hash = &candidate.hash;
                    #[cfg(feature = "rai_protocol")]
                    let is_votable = self.vote_type == VoteType::NonFinal
                        || spacing.votable(root, hash, self.clock.now());
                    #[cfg(not(feature = "rai_protocol"))]
                    let is_votable = spacing.votable(root, hash, self.clock.now());
                    if is_votable {
                        roots.push(*root);
                        hashes.push(*hash);
                        #[cfg(feature = "rai_protocol")]
                        epochs.push(candidate.epoch);
                        #[cfg(feature = "rai_protocol")]
                        qualified_roots.push(candidate.qualified_root.clone());
                    } else {
                        self.stats
                            .inc(self.stat_type(), DetailType::GeneratorSpacing);
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
                self.vote(
                    &hashes,
                    &roots,
                    #[cfg(feature = "rai_protocol")]
                    &qualified_roots,
                    #[cfg(feature = "rai_protocol")]
                    &epochs,
                    true,
                    |vote, _origin| {
                        #[cfg(feature = "rai_protocol")]
                        if _origin == VoteOrigin::Replay
                            && !self.reply_replay_filter.lock().unwrap().should_send(
                                request.channel.channel_id(),
                                &vote.signature,
                                Instant::now(),
                            )
                        {
                            self.stats
                                .inc(self.stat_type(), DetailType::GeneratorReplaySuppressed);
                            return;
                        }
                        #[cfg(feature = "rai_protocol")]
                        self.vote_broadcaster.reply_recovery(
                            vote,
                            request.channel.channel_id(),
                            _origin == VoteOrigin::Signed,
                        );
                        #[cfg(not(feature = "rai_protocol"))]
                        self.message_sender.lock().unwrap().try_send(
                            &request.channel,
                            &Message::ConfirmAck(ConfirmAck::new_with_own_vote((*vote).clone())),
                            TrafficType::Vote,
                        );
                        self.stats.inc_dir(
                            StatType::Requests,
                            DetailType::RequestsGeneratedVotes,
                            Direction::In,
                        );
                    },
                );
            }
        }
        self.stats
            .inc(self.stat_type(), DetailType::GeneratorReplies);
    }

    fn process_batch(&self, batch: VecDeque<VoteCandidate>) {
        let candidates = batch.into_iter().collect::<Vec<_>>();
        #[cfg(not(feature = "rai_protocol"))]
        let verified = self.ledger.verify_votes(
            candidates.iter().map(|c| (c.root, c.hash)).collect(),
            self.is_final,
        );
        // RAI permits extending a notarized selected chain and a final certificate finalizes
        // its unfinalized selected ancestors. Nano's vote verifier requires cemented
        // dependencies for both of its modes, so it is not the admissibility rule for any RAI
        // phase. At this layer require the exact candidate block to be locally available; RAI
        // phase history and conflict checks below remain the signing-safety gate.
        #[cfg(feature = "rai_protocol")]
        let verified: VecDeque<_> = {
            let any = self.ledger.any();
            candidates
                .iter()
                .filter(|candidate| {
                    any.get_block(&candidate.hash)
                        .is_some_and(|block| block.root() == candidate.root)
                })
                .map(|candidate| (candidate.root, candidate.hash))
                .collect()
        };
        let verified = verified
            .into_iter()
            .filter_map(|(root, hash)| {
                candidates
                    .iter()
                    .find(|candidate| candidate.root == root && candidate.hash == hash)
                    .cloned()
            })
            .collect::<Vec<_>>();

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
    candidates: VecDeque<VoteCandidate>,
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
mod tests {
    use super::*;

    fn recovery_fixture() -> (VoteGenerator, Arc<Channel>, Arc<rsnano_output_tracker::OutputTrackerMt<crate::transport::SendEvent>>, rsnano_types::PrivateKey) {
        use crate::{consensus::{AecService, VoteProcessorConfig}, representatives::RepresentativeTracker,
            transport::MessageFlooder};
        use rsnano_types::{Amount, NetworkType, PrivateKey, WalletId};
        let key = PrivateKey::from(1);
        let wallets = Arc::new(rsnano_wallet::Wallets::new_null());
        let wallet = WalletId::from(1);
        wallets.create(wallet);
        wallets.insert_adhoc2(&wallet, &key.raw_key(), false).unwrap();
        let mut reps = WalletRepresentatives::new(true, Amount::ZERO,
            Arc::new(rsnano_ledger::RepWeightCache::default()), wallets,
            Arc::new(RepresentativeTracker::new_null()));
        reps.compute_reps();
        let mut network = rsnano_network::Network::new_null();
        let channel = network.add_test_channel();
        let sender = MessageSender::new_null();
        let output = sender.track();
        let stats = Arc::new(Stats::default());
        let flooder = MessageFlooder::new(Arc::new(RepresentativeTracker::new_null()),
            Arc::new(std::sync::RwLock::new(network)), stats.clone(), sender);
        let broadcaster = VoteBroadcaster::new(Arc::new(crate::consensus::VoteProcessorQueue::new(
            VoteProcessorConfig::new(1), stats.clone())), flooder, stats.clone());
        let generator = VoteGenerator::new(Arc::new(Ledger::new_null()),
            Arc::new(Mutex::new(reps)), Arc::new(LocalVoteHistory::new(NetworkType::NanoDevNetwork)),
            true, stats, MessageSender::new_null(), Duration::ZERO, Duration::ZERO,
            Arc::new(broadcaster), Arc::new(SteadyClock::new_null()), VoteType::Final,
            Arc::new(VoteGate::default()), Arc::new(AecService::new_null()));
        (generator, channel, output, key)
    }

    #[test]
    fn election_request_replays_all_signed_phases_from_earliest_active_epoch() {
        let (generator, channel, output, key) = recovery_fixture();
        let block = rsnano_types::TestBlockBuilder::legacy_change().build();
        let slot = block.qualified_root().slot();
        for epoch in [3, 1] {
            generator.shared_state.active_elections.insert_vote_recovery(block.clone(), epoch);
            for phase in [VoteType::First, VoteType::NonFinal, VoteType::Timeout] {
                let vote = Arc::new(Vote::new_rai(&key, epoch, phase, vec![block.hash()]));
                generator.shared_state.history.add(&slot.root, &block.hash(), &vote);
            }
        }
        generator.shared_state.vote_gate.pause();
        generator.reply_earliest_election(slot, &channel);
        let messages = output.output();
        assert_eq!(messages.len(), 3);
        let phases: Vec<_> = messages.iter().map(|event| {
            let Message::ConfirmAck(ack) = &event.message else { panic!("expected vote") };
            assert_eq!(ack.vote().epoch(), 1);
            ack.vote().vote_type()
        }).collect();
        assert_eq!(phases, vec![VoteType::First, VoteType::NonFinal, VoteType::Timeout]);
        assert!(generator.shared_state.queues.lock().unwrap().requests.is_empty());
        generator.reply_earliest_election(rsnano_types::SlotRoot { previous: 999.into(), ..slot }, &channel);
        assert_eq!(output.output().len(), 3);
    }

    #[test]
    fn finalized_election_returns_only_earliest_final_vote_after_sealing() {
        let (generator, channel, output, key) = recovery_fixture();
        let block = rsnano_types::TestBlockBuilder::legacy_change().build();
        let slot = block.qualified_root().slot();
        let aec = &generator.shared_state.active_elections;
        aec.insert_vote_recovery(block.clone(), 1);
        for epoch in [1, 3] {
            for phase in [VoteType::First, VoteType::Final] {
                let vote = Arc::new(Vote::new_rai(&key, epoch, phase, vec![block.hash()]));
                generator.shared_state.history.add(&slot.root, &block.hash(), &vote);
            }
        }
        aec.merge_finalized_for_epoch(1, [(slot, block.hash())].into());
        aec.seal_finalized_epoch(1);
        aec.remove_epoch_elections(1);
        generator.shared_state.vote_gate.pause();
        generator.reply_earliest_election(slot, &channel);
        let messages = output.output();
        assert_eq!(messages.len(), 1);
        let Message::ConfirmAck(ack) = &messages[0].message else { panic!("expected vote") };
        assert_eq!(ack.vote().epoch(), 1);
        assert_eq!(ack.vote().vote_type(), VoteType::Final);
    }

    #[test]
    fn sealed_fast_finalization_can_generate_missing_final_reply() {
        let (generator, channel, output, _) = recovery_fixture();
        let block = rsnano_types::TestBlockBuilder::legacy_change().build();
        let slot = block.qualified_root().slot();
        let aec = &generator.shared_state.active_elections;
        aec.insert_vote_recovery(block.clone(), 2);
        aec.merge_finalized_for_epoch(2, [(slot, block.hash())].into());
        aec.seal_finalized_epoch(2);
        aec.remove_epoch_elections(2);
        generator.shared_state.vote_gate.pause();
        generator.reply_earliest_election(slot, &channel);
        let request = generator.shared_state.queues.lock().unwrap().requests.pop_front().unwrap();
        generator.shared_state.reply(request);
        let messages = output.output();
        assert_eq!(messages.len(), 1);
        let Message::ConfirmAck(ack) = &messages[0].message else { panic!("expected vote") };
        assert_eq!(ack.vote().epoch(), 2);
        assert_eq!(ack.vote().vote_type(), VoteType::Final);
        assert_eq!(ack.vote().hashes, vec![block.hash()]);
        assert!(ack.vote().validate().is_ok());
    }

    #[test]
    fn fresh_second_look_rechecks_strict_threshold_but_cached_replay_does_not() {
        use crate::{consensus::{AecService, ApplyVoteArgs, FilteredVote, ReceivedVote},
            representatives::{QuorumSnapshot, RepresentativeTracker}};
        use rsnano_ledger::{RepWeightCache, RepWeights};
        use rsnano_types::{Amount, NetworkType, PrivateKey, TestBlockBuilder, VoteDelivery, WalletId};
        use rsnano_wallet::Wallets;

        let rep = PrivateKey::from(1);
        let wallets = Arc::new(Wallets::new_null());
        let wallet = WalletId::from(1);
        wallets.create(wallet);
        wallets.insert_adhoc2(&wallet, &rep.raw_key(), false).unwrap();
        let rep_weights = Arc::new(RepWeightCache::default());
        let mut reps = WalletRepresentatives::new(true, Amount::ZERO, rep_weights,
            wallets, Arc::new(RepresentativeTracker::new_null()));
        reps.compute_reps();
        let aec = Arc::new(AecService::new_null());
        let block = TestBlockBuilder::legacy_change().build();
        let hash = block.hash();
        let root = block.qualified_root().with_epoch(1);
        aec.insert_vote_recovery(block, 1);
        let history = Arc::new(LocalVoteHistory::new(NetworkType::NanoDevNetwork));
        let first = Arc::new(Vote::new_rai(&rep, 1, VoteType::First, vec![hash]));
        history.add(&root.root, &hash, &first);
        let generator = VoteGenerator::new(
            Arc::new(Ledger::new_null()), Arc::new(Mutex::new(reps)), history,
            false, Arc::new(Stats::default()), MessageSender::new_null(),
            Duration::ZERO, Duration::ZERO, Arc::new(VoteBroadcaster::new_null()),
            Arc::new(SteadyClock::new_null()), VoteType::NonFinal,
            Arc::new(VoteGate::default()), aec.clone(),
        );
        let quorum = QuorumSnapshot::new_test_instance();
        let mut weights = RepWeights::default();
        weights.put(rep.public_key(), quorum.faulty_weight + quorum.slack_weight);
        let first: FilteredVote = ReceivedVote::new(first, VoteDelivery::Direct, None).into();
        aec.apply_vote(ApplyVoteArgs { vote: &first, rep_weights: &weights,
            quorum_snapshot: &quorum, now: rsnano_nullable_clock::Timestamp::new_test_instance() });
        let emitted = Mutex::new(Vec::new());
        let generate = |replay| generator.shared_state.vote(
            &[hash], &[root.root], &[root.clone()], &[1], replay,
            |vote, _| emitted.lock().unwrap().push(vote),
        );
        for replay in [false, true] {
            generate(replay);
            assert!(emitted.lock().unwrap().is_empty(), "exactly f+p must not second-look");
        }
        // One additional First vote crosses the strict threshold.
        let other = PrivateKey::from(2);
        weights.put(other.public_key(), Amount::from(1));
        let vote: FilteredVote = ReceivedVote::new(
            Arc::new(Vote::new_rai(&other, 1, VoteType::First, vec![hash])),
            VoteDelivery::Direct, None,
        ).into();
        aec.apply_vote(ApplyVoteArgs { vote: &vote, rep_weights: &weights,
            quorum_snapshot: &quorum, now: rsnano_nullable_clock::Timestamp::new_test_instance() });
        generate(true);
        assert_eq!(emitted.lock().unwrap().len(), 1);
        aec.erase(&root);
        generate(true);
        let emitted = emitted.lock().unwrap();
        assert_eq!(emitted.len(), 2);
        assert_eq!(emitted[0].signature, emitted[1].signature);
    }

    #[test]
    fn suppresses_recent_replay_to_same_channel() {
        let now = Instant::now();
        let mut filter = ReplayFilter::new(Duration::from_millis(500));
        let signature = Signature::from_bytes([1; 64]);

        assert!(filter.should_send(ChannelId::from(1), &signature, now));
        assert!(!filter.should_send(
            ChannelId::from(1),
            &signature,
            now + Duration::from_millis(499)
        ));
    }

    #[test]
    fn permits_replay_to_different_channel() {
        let now = Instant::now();
        let mut filter = ReplayFilter::new(Duration::from_millis(500));
        let signature = Signature::from_bytes([1; 64]);

        assert!(filter.should_send(ChannelId::from(1), &signature, now));
        assert!(filter.should_send(ChannelId::from(2), &signature, now));
    }

    #[test]
    fn permits_retry_after_interval() {
        let now = Instant::now();
        let mut filter = ReplayFilter::new(Duration::from_millis(500));
        let signature = Signature::from_bytes([1; 64]);

        assert!(filter.should_send(ChannelId::from(1), &signature, now));
        assert!(filter.should_send(
            ChannelId::from(1),
            &signature,
            now + Duration::from_millis(500)
        ));
    }
}
