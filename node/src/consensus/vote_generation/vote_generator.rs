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

#[cfg(feature = "rai_protocol")]
use std::collections::HashSet;

use rsnano_ledger::{AnySet, Ledger};
use rsnano_messages::{ConfirmAck, Message};
use rsnano_network::{Channel, ChannelId, TrafficType};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{BlockHash, Root, SavedBlock, UnixMillisTimestamp, Vote};
#[cfg(feature = "rai_protocol")]
use rsnano_types::{PublicKey, RaiVoteMetadata, RaiVotePhase};
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
    candidates: Vec<VoteCandidate>,
    pub channel: Arc<Channel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VoteCandidate {
    root: Root,
    hash: BlockHash,
    #[cfg(feature = "rai_protocol")]
    metadata: RaiVoteMetadata,
    #[cfg(feature = "rai_protocol")]
    is_rai_close: bool,
}

impl VoteCandidate {
    fn same_context(&self, _other: &Self) -> bool {
        #[cfg(feature = "rai_protocol")]
        return self.metadata == _other.metadata;

        #[cfg(not(feature = "rai_protocol"))]
        true
    }
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
    pub(crate) fn all_local_reps_voted_first(
        &self,
        root: &Root,
        metadata: &RaiVoteMetadata,
    ) -> bool {
        let mut first = metadata.clone();
        first.phase = RaiVotePhase::First;
        self.shared_state
            .wallet_reps
            .lock()
            .unwrap()
            .rep_pub_keys()
            .all(|voter| {
                self.shared_state.history.rai_phase_vote_exists(
                    root,
                    &BlockHash::ZERO,
                    &voter,
                    &first,
                )
            })
    }

    /// Generates a signed response for a contextless representative-crawler
    /// query. Discovery votes are sent only to the requesting channel and are
    /// deliberately excluded from local vote history and vote spacing; the
    /// receiver rejects their reserved metadata before RAI consensus handling.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn generate_rai_discovery_votes(
        &self,
        blocks: &[SavedBlock],
        channel: &Arc<Channel>,
    ) -> usize {
        let rep_keys = self.shared_state.rai_rep_keys();
        if rep_keys.is_empty() {
            return 0;
        }

        for chunk in blocks.chunks(Self::MAX_HASHES) {
            let hashes = chunk.iter().map(|block| block.hash()).collect::<Vec<_>>();
            self.stats.add_dir(
                StatType::Requests,
                DetailType::RequestsGeneratedHashes,
                Direction::In,
                hashes.len() as u64,
            );

            for rep_key in &rep_keys {
                let vote = Vote::new_rai(
                    rep_key,
                    UnixMillisTimestamp::now(),
                    0x9, // 8192ms
                    hashes.clone(),
                    RaiVoteMetadata::default(),
                );
                let confirm = Message::ConfirmAck(ConfirmAck::new_with_own_vote(vote));
                self.shared_state.message_sender.lock().unwrap().try_send(
                    channel,
                    &confirm,
                    TrafficType::VoteReply,
                );
                self.stats.inc_dir(
                    StatType::Requests,
                    DetailType::RequestsGeneratedVotes,
                    Direction::In,
                );
            }
        }

        blocks.len()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn generate_rai_notar_vote(
        &self,
        target: &rsnano_ledger::RaiFinalizedVoteTarget,
        channel: &Arc<Channel>,
    ) -> usize {
        let generated = std::sync::atomic::AtomicUsize::new(0);
        self.shared_state.vote(
            &[target.hash],
            &[target.root],
            target.metadata.clone(),
            false,
            |vote| {
                generated.fetch_add(1, Ordering::Relaxed);
                let message = Message::ConfirmAck(ConfirmAck::new_with_own_vote((*vote).clone()));
                self.shared_state.message_sender.lock().unwrap().try_send(
                    channel,
                    &message,
                    TrafficType::VoteReply,
                );
            },
        );
        generated.load(Ordering::Relaxed)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn generate_rai_final_vote(
        &self,
        target: &rsnano_ledger::RaiFinalizedVoteTarget,
        channel: &Arc<Channel>,
    ) -> usize {
        let generated = std::sync::atomic::AtomicUsize::new(0);
        self.shared_state.vote(
            &[target.hash],
            &[target.root],
            target.metadata.clone(),
            true,
            |vote| {
                generated.fetch_add(1, Ordering::Relaxed);
                let message = Message::ConfirmAck(ConfirmAck::new_with_own_vote((*vote).clone()));
                self.shared_state.message_sender.lock().unwrap().try_send(
                    channel,
                    &message,
                    TrafficType::VoteReply,
                );
            },
        );
        generated.load(Ordering::Relaxed)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn reply_cached_rai_votes(
        &self,
        root: &rsnano_types::Root,
        hash: &BlockHash,
        metadata: &RaiVoteMetadata,
        channel: &Arc<Channel>,
    ) -> usize {
        let votes = self
            .shared_state
            .history
            .votes(root, hash, false)
            .into_iter()
            .filter(|vote| {
                vote.metadata.election_id == metadata.election_id
                    && vote.metadata.epoch == metadata.epoch
            })
            .collect::<Vec<_>>();
        for vote in &votes {
            let confirm = Message::ConfirmAck(ConfirmAck::new_with_own_vote((**vote).clone()));
            self.shared_state.message_sender.lock().unwrap().try_send(
                channel,
                &confirm,
                TrafficType::VoteReply,
            );
        }
        votes.len()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn reply_cached_rai_election_votes(
        &self,
        metadata: &RaiVoteMetadata,
        channel: &Arc<Channel>,
    ) -> usize {
        let votes = self
            .shared_state
            .history
            .rai_votes()
            .into_iter()
            .filter(|vote| vote.metadata.election_id == metadata.election_id)
            .collect::<Vec<_>>();
        for vote in &votes {
            let confirm = Message::ConfirmAck(ConfirmAck::new_with_own_vote((**vote).clone()));
            self.shared_state.message_sender.lock().unwrap().try_send(
                channel,
                &confirm,
                TrafficType::VoteReply,
            );
        }
        votes.len()
    }

    const MAX_REQUESTS: usize = 2048;
    const MAX_HASHES: usize = 255;

    pub(crate) fn new(
        ledger: Arc<Ledger>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        history: Arc<LocalVoteHistory>,
        #[cfg(feature = "rai_protocol")] rai_signing_lock: Arc<Mutex<()>>,
        is_final: bool,
        stats: Arc<Stats>,
        message_sender: MessageSender,
        voting_delay: Duration,
        vote_generator_delay: Duration,
        vote_broadcaster: Arc<VoteBroadcaster>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        let shared_state = Arc::new(SharedState {
            ledger: Arc::clone(&ledger),
            message_sender: Mutex::new(message_sender),
            history,
            #[cfg(feature = "rai_protocol")]
            rai_signing_lock,
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
            rai_rep_keys: Mutex::new(Vec::new()),
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
    pub(crate) fn add(
        &self,
        root: &Root,
        hash: &BlockHash,
        #[cfg(feature = "rai_protocol")] metadata: RaiVoteMetadata,
        #[cfg(feature = "rai_protocol")] is_rai_close: bool,
    ) {
        self.vote_generation_queue.add(VoteCandidate {
            root: *root,
            hash: *hash,
            #[cfg(feature = "rai_protocol")]
            metadata,
            #[cfg(feature = "rai_protocol")]
            is_rai_close,
        });
    }

    /// Queue blocks for vote generation, returning the number of successful candidates.
    pub(crate) fn generate(
        &self,
        blocks: &[SavedBlock],
        channel: &Arc<Channel>,
        #[cfg(feature = "rai_protocol")] contexts: &[(RaiVoteMetadata, bool)],
    ) -> usize {
        #[cfg(feature = "rai_protocol")]
        let mut cached = 0;
        #[cfg(feature = "rai_protocol")]
        let mut sent_cached = HashSet::new();
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
                .enumerate()
                .filter_map(|(_index, i)| {
                    #[cfg(feature = "rai_protocol")]
                    {
                        let context = &contexts[_index].0;
                        let votes = self
                            .shared_state
                            .history
                            .votes(&i.root(), &i.hash(), false)
                            .into_iter()
                            .filter(|vote| {
                                // ConfirmReq does not carry RAI metadata. If
                                // this node no longer has the requested
                                // election, replay its signed persistent vote
                                // history for the requested root/hash instead
                                // of generating an unusable all-zero-context
                                // vote. A live election still narrows replay
                                // to its exact governing context.
                                context == &RaiVoteMetadata::default()
                                    || (vote.metadata.election_id == context.election_id
                                        && vote.metadata.epoch == context.epoch)
                            })
                            .collect::<Vec<_>>();
                        if !votes.is_empty() {
                            for vote in votes {
                                // Vote::hash() covers the signed payload, not
                                // the representative identity. Every signer
                                // of the same phase/value is distinct
                                // certificate evidence and must be replayed.
                                if !sent_cached.insert((vote.voter, vote.hash())) {
                                    continue;
                                }
                                if std::env::var_os("RSNANO_RAI_TRACE_PR").is_some() {
                                    eprintln!(
                                        "RAI_SOLICIT_TRACE send_cached_vote channel={:?} vote={:?}",
                                        channel.channel_id(),
                                        vote
                                    );
                                }
                                let confirm = Message::ConfirmAck(ConfirmAck::new_with_own_vote(
                                    (*vote).clone(),
                                ));
                                self.shared_state.message_sender.lock().unwrap().try_send(
                                    channel,
                                    &confirm,
                                    TrafficType::Vote,
                                );
                                self.stats.inc_dir(
                                    StatType::Requests,
                                    DetailType::RequestsGeneratedVotes,
                                    Direction::In,
                                );
                            }
                            // A cached RAI vote is evidence, but it does not
                            // necessarily cover the election's current phase.
                            // When the election is still live, keep the
                            // candidate so this node's representative can
                            // generate the currently required vote as well as
                            // replaying its retained evidence. Contextless
                            // ConfirmReq repair can only replay history safely.
                            if context == &RaiVoteMetadata::default() {
                                cached += 1;
                                return None;
                            }
                        }
                        if context == &RaiVoteMetadata::default() {
                            return None;
                        }
                    }
                    if can_vote(i) {
                        Some(VoteCandidate {
                            root: i.root(),
                            hash: i.hash(),
                            #[cfg(feature = "rai_protocol")]
                            metadata: contexts[_index].0.clone(),
                            #[cfg(feature = "rai_protocol")]
                            is_rai_close: contexts[_index].1,
                        })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };

        let result = req_candidates.len();
        #[cfg(feature = "rai_protocol")]
        let result = result + cached;
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

/// Computes the phase this representative may sign for a RAI election.
/// Second-look Notar votes require a prior First vote by the same signer, and
/// a signer which already emitted Final must not return to an earlier phase.
#[cfg(feature = "rai_protocol")]
fn rai_signing_metadata(
    history: &LocalVoteHistory,
    roots: &[Root],
    hashes: &[BlockHash],
    voter: &PublicKey,
    mut metadata: RaiVoteMetadata,
    is_final_generator: bool,
) -> Option<RaiVoteMetadata> {
    metadata.phase = if is_final_generator {
        RaiVotePhase::Final
    } else if metadata.phase == RaiVotePhase::Notar {
        RaiVotePhase::Notar
    } else {
        RaiVotePhase::First
    };

    if is_final_generator {
        if roots
            .iter()
            .zip(hashes)
            .any(|(root, hash)| !history.rai_support_is_compatible(root, hash, voter, &metadata))
        {
            return None;
        }
        return Some(metadata);
    }

    let mut final_metadata = metadata.clone();
    final_metadata.phase = RaiVotePhase::Final;
    if roots
        .iter()
        .any(|root| history.rai_phase_vote_exists(root, &BlockHash::ZERO, voter, &final_metadata))
    {
        return None;
    }

    if metadata.phase == RaiVotePhase::Notar {
        let mut first_metadata = metadata.clone();
        first_metadata.phase = RaiVotePhase::First;
        if roots.iter().any(|root| {
            !history.rai_phase_vote_exists(root, &BlockHash::ZERO, voter, &first_metadata)
        }) {
            metadata.phase = RaiVotePhase::First;
        }
    }

    Some(metadata)
}

struct SharedState {
    ledger: Arc<Ledger>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    history: Arc<LocalVoteHistory>,
    #[cfg(feature = "rai_protocol")]
    rai_signing_lock: Arc<Mutex<()>>,
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
    rai_rep_keys: Mutex<Vec<rsnano_types::PrivateKey>>,
}

impl SharedState {
    #[cfg(feature = "rai_protocol")]
    fn rai_rep_keys(&self) -> Vec<rsnano_types::PrivateKey> {
        let mut cached = self.rai_rep_keys.lock().unwrap();
        if cached.is_empty() {
            self.wallet_reps.lock().unwrap().rep_priv_keys(&mut cached);
        }
        cached.clone()
    }

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
                #[cfg(feature = "rai_protocol")]
                {
                    // RAI metadata is signed and unique per election, so its
                    // candidates cannot be vectorized. Drain the next context
                    // immediately instead of imposing the legacy batching
                    // delay on every individual slot.
                    queues.next_broadcast = if queues.candidates.is_empty() {
                        Instant::now() + self.vote_generator_delay
                    } else {
                        Instant::now()
                    };
                }
                #[cfg(not(feature = "rai_protocol"))]
                {
                    queues.next_broadcast = Instant::now() + self.vote_generator_delay;
                }
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
        let context = queues.candidates.front().cloned();
        {
            let spacing = self.spacing.lock().unwrap();
            while queues
                .candidates
                .front()
                .is_some_and(|candidate| context.as_ref().unwrap().same_context(candidate))
            {
                let candidate = queues.candidates.pop_front().unwrap();
                let root = candidate.root;
                let hash = candidate.hash;
                if !roots.contains(&root) {
                    if spacing.votable(&root, &hash, self.clock.now()) {
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
            self.vote(
                &hashes,
                &roots,
                #[cfg(feature = "rai_protocol")]
                context.unwrap().metadata,
                #[cfg(feature = "rai_protocol")]
                false,
                |generated_vote| {
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
        #[cfg(feature = "rai_protocol")] metadata: RaiVoteMetadata,
        #[cfg(feature = "rai_protocol")] regenerate_finalized: bool,
        action: F,
    ) where
        F: Fn(Arc<Vote>),
    {
        debug_assert_eq!(hashes.len(), roots.len());
        #[cfg(not(feature = "rai_protocol"))]
        let mut rep_keys = {
            let mut keys = Vec::new();
            self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
            keys
        };
        #[cfg(feature = "rai_protocol")]
        let mut rep_keys = self.rai_rep_keys();
        #[cfg(feature = "rai_protocol")]
        let rai_signing_guard = self.rai_signing_lock.lock().unwrap();

        let mut votes = Vec::new();
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
            #[cfg(feature = "rai_protocol")]
            {
                let Some(metadata) = rai_signing_metadata(
                    &self.history,
                    roots,
                    hashes,
                    &rep_key.public_key(),
                    metadata.clone(),
                    self.is_final,
                ) else {
                    continue;
                };
                // An explicit repair request for a durably finalized target
                // must always receive a freshly signed reply. The immutable
                // target fixes the election, phase, and value, so bypassing
                // live phase-slot suppression cannot authorize equivocation.
                if !regenerate_finalized
                    && roots.iter().zip(hashes).any(|(root, hash)| {
                        self.history.rai_phase_vote_exists(
                            root,
                            hash,
                            &rep_key.public_key(),
                            &metadata,
                        )
                    })
                {
                    continue;
                }
                votes.push(Arc::new(Vote::new_rai(
                    &rep_key,
                    timestamp,
                    duration,
                    hashes.to_vec(),
                    metadata,
                )));
            }
            #[cfg(not(feature = "rai_protocol"))]
            votes.push(Arc::new(Vote::new(
                &rep_key,
                timestamp,
                duration,
                hashes.to_vec(),
            )));
        }

        let record_vote = |vote: &Arc<Vote>| {
            let now = self.clock.now();
            let mut spacing = self.spacing.lock().unwrap();
            for i in 0..hashes.len() {
                self.history.add(&roots[i], &hashes[i], &vote);
                spacing.flag(&roots[i], &hashes[i], now);
            }
        };

        #[cfg(feature = "rai_protocol")]
        {
            // Phase selection and history insertion are one atomic local
            // signing decision shared by the final and non-final generators.
            for vote in &votes {
                record_vote(vote);
            }
            drop(rai_signing_guard);
            for vote in votes {
                action(vote);
            }
        }

        #[cfg(not(feature = "rai_protocol"))]
        for vote in votes {
            record_vote(&vote);
            action(vote);
        }
    }

    fn reply(&self, request: VoteRequest) {
        let mut i = request.candidates.iter().peekable();
        while i.peek().is_some() && !self.stopped.load(Ordering::SeqCst) {
            let mut hashes = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            let mut roots = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            #[cfg(feature = "rai_protocol")]
            let context = i.peek().unwrap().metadata.clone();
            {
                let spacing = self.spacing.lock().unwrap();
                while hashes.len() < VoteGenerator::MAX_HASHES {
                    #[cfg(feature = "rai_protocol")]
                    if i.peek()
                        .is_some_and(|candidate| candidate.metadata != context)
                    {
                        break;
                    }
                    let Some(candidate) = i.next() else {
                        break;
                    };
                    if !roots.contains(&candidate.root) {
                        if spacing.votable(&candidate.root, &candidate.hash, self.clock.now()) {
                            roots.push(candidate.root);
                            hashes.push(candidate.hash);
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
                self.vote(
                    &hashes,
                    &roots,
                    #[cfg(feature = "rai_protocol")]
                    context,
                    #[cfg(feature = "rai_protocol")]
                    false,
                    |vote| {
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
                    },
                );
            }
        }
        self.stats
            .inc(self.stat_type(), DetailType::GeneratorReplies);
    }

    fn process_batch(&self, batch: VecDeque<VoteCandidate>) {
        let pairs = batch
            .iter()
            .map(|candidate| (candidate.root, candidate.hash))
            .collect();
        let verified = self.ledger.verify_votes(pairs, self.is_final);
        let verified = batch
            .into_iter()
            .filter(|candidate| {
                #[cfg(feature = "rai_protocol")]
                if candidate.is_rai_close || candidate.hash.is_zero() {
                    return true;
                }
                verified.contains(&(candidate.root, candidate.hash))
            })
            .collect::<VecDeque<_>>();

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
mod rai_signing_tests {
    use super::*;
    use rsnano_types::{
        PrivateKey, QualifiedRoot, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId,
    };

    fn metadata(slot_root: Root, phase: RaiVotePhase) -> RaiVoteMetadata {
        RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::ZERO,
                root: QualifiedRoot::new(slot_root, BlockHash::from(99)),
            }),
            phase,
            epoch: RaiEpoch::ZERO,
            scope: RaiCommitteeScope::All,
        }
    }

    fn remember(
        history: &LocalVoteHistory,
        key: &PrivateKey,
        root: Root,
        hash: BlockHash,
        metadata: RaiVoteMetadata,
    ) {
        let vote = Arc::new(Vote::new_rai(
            key,
            UnixMillisTimestamp::new(16),
            0,
            vec![hash],
            metadata,
        ));
        history.add(&root, &hash, &vote);
    }

    #[test]
    fn notar_requires_first_from_each_signer_for_the_same_election() {
        let history = LocalVoteHistory::with_max_cache(32);
        let first_signer = PrivateKey::new();
        let missing_signer = PrivateKey::new();
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let requested = metadata(root, RaiVotePhase::Notar);

        assert_eq!(
            rai_signing_metadata(
                &history,
                &[root],
                &[hash],
                &first_signer.public_key(),
                requested.clone(),
                false,
            )
            .unwrap()
            .phase,
            RaiVotePhase::First
        );

        // A First vote for another election does not unlock this Notar slot.
        remember(
            &history,
            &first_signer,
            root,
            hash,
            metadata(Root::from(77), RaiVotePhase::First),
        );
        assert_eq!(
            rai_signing_metadata(
                &history,
                &[root],
                &[hash],
                &first_signer.public_key(),
                requested.clone(),
                false,
            )
            .unwrap()
            .phase,
            RaiVotePhase::First
        );

        remember(
            &history,
            &first_signer,
            root,
            hash,
            metadata(root, RaiVotePhase::First),
        );
        assert_eq!(
            rai_signing_metadata(
                &history,
                &[root],
                &[hash],
                &first_signer.public_key(),
                requested.clone(),
                false,
            )
            .unwrap()
            .phase,
            RaiVotePhase::Notar
        );
        assert_eq!(
            rai_signing_metadata(
                &history,
                &[root],
                &[hash],
                &missing_signer.public_key(),
                requested,
                false,
            )
            .unwrap()
            .phase,
            RaiVotePhase::First
        );
    }

    #[test]
    fn final_locks_earlier_phases_but_does_not_require_slot_first() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let requested = metadata(root, RaiVotePhase::Notar);

        assert_eq!(
            rai_signing_metadata(
                &history,
                &[root],
                &[hash],
                &signer.public_key(),
                requested.clone(),
                true,
            )
            .unwrap()
            .phase,
            RaiVotePhase::Final
        );

        remember(
            &history,
            &signer,
            root,
            hash,
            metadata(root, RaiVotePhase::Final),
        );
        assert!(
            rai_signing_metadata(
                &history,
                &[root],
                &[hash],
                &signer.public_key(),
                requested,
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn final_generation_rejects_conflicting_existing_support() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let requested = metadata(root, RaiVotePhase::Notar);

        remember(
            &history,
            &signer,
            root,
            BlockHash::ZERO,
            metadata(root, RaiVotePhase::First),
        );
        assert!(
            rai_signing_metadata(
                &history,
                &[root],
                &[hash],
                &signer.public_key(),
                requested,
                true,
            )
            .is_none()
        );
    }
}
