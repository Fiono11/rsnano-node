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
use rsnano_types::{BlockHash, Root, SavedBlock, UnixMillisTimestamp, Vote};
#[cfg(feature = "rai_protocol")]
use rsnano_types::{RaiVoteMetadata, RaiVotePhase};
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
    pub candidates: Vec<(Root, BlockHash)>,
    pub channel: Arc<Channel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    pub(crate) fn generate(&self, blocks: &[SavedBlock], channel: &Arc<Channel>) -> usize {
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
    rai_rep_keys: Mutex<Vec<rsnano_types::PrivateKey>>,
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
        let context = queues.candidates.front().copied();
        {
            let spacing = self.spacing.lock().unwrap();
            while queues
                .candidates
                .front()
                .is_some_and(|candidate| context.unwrap().same_context(candidate))
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
        #[cfg(feature = "rai_protocol")] mut metadata: RaiVoteMetadata,
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
        let mut rep_keys = {
            let mut cached = self.rai_rep_keys.lock().unwrap();
            if cached.is_empty() {
                self.wallet_reps.lock().unwrap().rep_priv_keys(&mut cached);
            }
            cached.clone()
        };

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
                metadata.phase = if self.is_final {
                    RaiVotePhase::Final
                } else {
                    RaiVotePhase::First
                };
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

        for vote in votes {
            {
                let now = self.clock.now();
                let mut spacing = self.spacing.lock().unwrap();
                for i in 0..hashes.len() {
                    self.history.add(&roots[i], &hashes[i], &vote);
                    spacing.flag(&roots[i], &hashes[i], now);
                }
            }
            action(vote);
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
                        if spacing.votable(root, hash, self.clock.now()) {
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
                self.vote(
                    &hashes,
                    &roots,
                    #[cfg(feature = "rai_protocol")]
                    RaiVoteMetadata::default(),
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
                if candidate.is_rai_close {
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
