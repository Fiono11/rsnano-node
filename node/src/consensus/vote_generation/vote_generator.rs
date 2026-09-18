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
use rsnano_types::{BlockHash, ConsensusEpoch, Root, SavedBlock, Vote, VoteKind};
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
    /// RAI: the epoch the requester's election runs in
    pub epoch: ConsensusEpoch,
    pub channel: Arc<Channel>,
}

/// A block to vote for in one consensus epoch
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VoteCandidate {
    pub root: Root,
    pub hash: BlockHash,
    pub epoch: ConsensusEpoch,
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
        kind: VoteKind,
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
            kind,
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
                Self::thread_name(kind),
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

    fn thread_name(kind: VoteKind) -> String {
        match kind {
            VoteKind::First => "Voting".to_owned(),
            VoteKind::Final => "Voting final".to_owned(),
            VoteKind::Notar => "Voting notar".to_owned(),
            VoteKind::Timeout => "Voting timeout".to_owned(),
            VoteKind::Abstain => "Voting abstain".to_owned(),
        }
    }

    pub(crate) fn start(&self) {
        let shared_state_clone = Arc::clone(&self.shared_state);
        *self.thread.lock().unwrap() = Some(
            thread::Builder::new()
                .name(Self::thread_name(self.shared_state.kind))
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
    pub(crate) fn add(&self, root: &Root, hash: &BlockHash, epoch: ConsensusEpoch) {
        self.vote_generation_queue.add(VoteCandidate {
            root: *root,
            hash: *hash,
            epoch,
        });
    }

    /// Queue blocks for vote generation in the given epoch, returning the
    /// number of successful candidates.
    pub(crate) fn generate(
        &self,
        blocks: &[SavedBlock],
        channel: &Arc<Channel>,
        epoch: ConsensusEpoch,
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
                            self.shared_state.kind.is_final()
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
            epoch,
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
            ("candidates", candidates_count, size_of::<VoteCandidate>()),
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
    kind: VoteKind,
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
        let mut hashes = Vec::with_capacity(VoteGenerator::MAX_HASHES);
        let mut roots = Vec::with_capacity(VoteGenerator::MAX_HASHES);
        // A vote is for one epoch: the batch takes the epoch of the first
        // candidate, the candidates of other epochs wait for the next batch
        let mut epoch = ConsensusEpoch::ZERO;
        let mut deferred = Vec::new();
        {
            let spacing = self.spacing.lock().unwrap();
            while let Some(candidate) = queues.candidates.pop_front() {
                if hashes.is_empty() && roots.is_empty() {
                    epoch = candidate.epoch;
                } else if candidate.epoch != epoch {
                    deferred.push(candidate);
                    continue;
                }
                let VoteCandidate { root, hash, .. } = candidate;
                if !roots.contains(&root) {
                    if self.skips_ledger_checks() || spacing.votable(&root, &hash, self.clock.now())
                    {
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
            for candidate in deferred.into_iter().rev() {
                queues.candidates.push_front(candidate);
            }
        }

        if !hashes.is_empty() {
            drop(queues);
            self.vote(&hashes, &roots, epoch, |generated_vote| {
                self.stats
                    .inc(self.stat_type(), DetailType::GeneratorBroadcasts);
                let sample = match self.kind {
                    VoteKind::First => Sample::VoteGeneratorHashes,
                    VoteKind::Final => Sample::VoteGeneratorFinalHashes,
                    VoteKind::Notar => Sample::VoteGeneratorNotarHashes,
                    VoteKind::Timeout => Sample::VoteGeneratorTimeoutHashes,
                    VoteKind::Abstain => Sample::VoteGeneratorAbstainHashes,
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

    fn vote<F>(&self, hashes: &[BlockHash], roots: &[Root], epoch: ConsensusEpoch, action: F)
    where
        F: Fn(Arc<Vote>),
    {
        debug_assert_eq!(hashes.len(), roots.len());
        let mut rep_keys = Vec::new();

        self.wallet_reps
            .lock()
            .unwrap()
            .rep_priv_keys(&mut rep_keys);

        let mut votes = Vec::new();
        for rep_key in rep_keys.drain(..) {
            votes.push(Arc::new(Vote::new_in_epoch(
                &rep_key,
                self.kind,
                epoch,
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
                self.vote(&hashes, &roots, request.epoch, |vote| {
                    // Kudzu: a reply is evidence handed over on request. The same
                    // (immutable) vote may have been sent before and lost, so it
                    // must not be dropped as a duplicate by the requester.
                    let ack = if cfg!(feature = "rai_protocol") {
                        ConfirmAck::new_with_certificate_evidence((*vote).clone())
                    } else {
                        ConfirmAck::new_with_own_vote((*vote).clone())
                    };
                    let confirm = Message::ConfirmAck(ack);
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

    /// Kudzu notarization and timeout votes may go to blocks which are not in
    /// our ledger (a second look at a fork), so they bypass the ledger checks
    /// and the vote spacing. The election already checked that we hold the
    /// block. RAI: every vote is a statement decided by an instance, the
    /// ledger's rules for legacy votes (no non-final vote for a cemented
    /// block, spacing per root) do not apply to any kind.
    fn skips_ledger_checks(&self) -> bool {
        cfg!(feature = "rai_protocol")
            || matches!(
                self.kind,
                VoteKind::Notar | VoteKind::Timeout | VoteKind::Abstain
            )
    }

    fn process_batch(&self, batch: VecDeque<VoteCandidate>) {
        let verified = if self.skips_ledger_checks() {
            batch
        } else {
            self.verify_votes(batch)
        };

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

    /// The ledger checks are per block, one epoch at a time
    fn verify_votes(&self, batch: VecDeque<VoteCandidate>) -> VecDeque<VoteCandidate> {
        let mut epochs = Vec::new();
        for candidate in &batch {
            if !epochs.contains(&candidate.epoch) {
                epochs.push(candidate.epoch);
            }
        }
        let mut verified = VecDeque::with_capacity(batch.len());
        for epoch in epochs {
            let pairs = batch
                .iter()
                .filter(|c| c.epoch == epoch)
                .map(|c| (c.root, c.hash))
                .collect();
            verified.extend(
                self.ledger
                    .verify_votes(pairs, self.kind.is_final())
                    .into_iter()
                    .map(|(root, hash)| VoteCandidate { root, hash, epoch }),
            );
        }
        verified
    }

    fn stat_type(&self) -> StatType {
        match self.kind {
            VoteKind::First => StatType::VoteGenerator,
            VoteKind::Final => StatType::VoteGeneratorFinal,
            VoteKind::Notar => StatType::VoteGeneratorNotar,
            VoteKind::Timeout => StatType::VoteGeneratorTimeout,
            VoteKind::Abstain => StatType::VoteGeneratorAbstain,
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
