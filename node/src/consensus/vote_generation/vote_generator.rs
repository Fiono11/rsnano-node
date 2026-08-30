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
use rsnano_types::{BlockHash, QualifiedRoot, Root, SavedBlock, Vote};
#[cfg(feature = "rai_protocol")]
use rsnano_types::{Signature, VoteType};
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
        #[cfg(feature = "rai_protocol")] vote_type: VoteType,
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
        });
    }

    /// Queue blocks for vote generation, returning the number of successful candidates.
    pub(crate) fn generate(
        &self,
        blocks: &[SavedBlock],
        channel: &Arc<Channel>,
        #[cfg(feature = "rai_protocol")] epoch: u64,
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
                        Some(VoteCandidate {
                            root: i.root(),
                            hash: i.hash(),
                            #[cfg(feature = "rai_protocol")]
                            epoch,
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
}

#[derive(Clone, Copy)]
pub(super) struct VoteCandidate {
    root: Root,
    hash: BlockHash,
    #[cfg(feature = "rai_protocol")]
    epoch: u64,
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
                if !roots.contains(&root) {
                    if spacing.votable(&root, &hash, self.clock.now()) {
                        roots.push(root);
                        hashes.push(hash);
                        #[cfg(feature = "rai_protocol")]
                        epochs.push(candidate.epoch);
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
        #[cfg(feature = "rai_protocol")] epochs: &[u64],
        resign_cached: bool,
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
                let mut pending_hashes = Vec::new();
                let mut pending_roots = Vec::new();
                let mut pending_epochs = Vec::new();
                for ((root, hash), epoch) in roots.iter().zip(hashes).zip(epochs) {
                    if !resign_cached
                        && self.history.has_vote_type(
                        root,
                        *epoch,
                        self.vote_type,
                        rep_key.public_key(),
                    )
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
                        && !self.history.has_no_conflicting_notarization(
                            root,
                            *epoch,
                            hash,
                            rep_key.public_key(),
                        )
                    {
                        self.stats.inc(
                            self.stat_type(),
                            DetailType::GeneratorHistorySuppressedConflict,
                        );
                        continue;
                    }
                    // A confirm_req reply must cover only the requested hashes. Replaying a
                    // cached batched RAI vote here amplifies one requested hash into as many
                    // as 255 unrelated hashes at every receiving node. Re-signing the requested
                    // subset is safe (the phase and hash are unchanged) and keeps reply work
                    // proportional to the request.
                    pending_roots.push(*root);
                    pending_hashes.push(*hash);
                    pending_epochs.push(*epoch);
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
            {
                let spacing = self.spacing.lock().unwrap();
                while hashes.len() < VoteGenerator::MAX_HASHES {
                    let Some(candidate) = i.next() else {
                        break;
                    };
                    let root = &candidate.root;
                    let hash = &candidate.hash;
                    if !roots.contains(root) {
                        if spacing.votable(root, hash, self.clock.now()) {
                            roots.push(*root);
                            hashes.push(*hash);
                            #[cfg(feature = "rai_protocol")]
                            epochs.push(candidate.epoch);
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
                    .copied()
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
