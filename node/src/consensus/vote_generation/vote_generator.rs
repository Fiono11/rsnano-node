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
use std::collections::{HashMap, HashSet};

use rsnano_ledger::{AnySet, Ledger};
use rsnano_messages::{ConfirmAck, Message};
use rsnano_network::{Channel, ChannelId, TrafficType};
use rsnano_nullable_clock::SteadyClock;
#[cfg(feature = "rai_protocol")]
use rsnano_nullable_clock::Timestamp;
#[cfg(feature = "rai_protocol")]
use rsnano_types::{
    Account, PublicKey, RaiCommitteeScope, RaiElectionId, RaiTimeoutSlot, RaiVoteMetadata,
    RaiVotePhase,
};
use rsnano_types::{BlockHash, Root, SavedBlock, UnixMillisTimestamp, Vote};
use rsnano_utils::{
    container_info::ContainerInfo,
    stats::{DetailType, Direction, Sample, StatType, Stats},
};

use super::LocalVoteHistory;
#[cfg(not(feature = "rai_protocol"))]
use super::VoteSpacing;
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

#[cfg(feature = "rai_protocol")]
fn same_rai_vote_context(first: &RaiVoteMetadata, second: &RaiVoteMetadata) -> bool {
    first.election_id == second.election_id && first.scope == second.scope
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
                let vote = Vote::new_rai_batch(
                    rep_key,
                    UnixMillisTimestamp::now(),
                    0x9, // 8192ms
                    hashes
                        .iter()
                        .map(|hash| (RaiVoteMetadata::default(), *hash)),
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
        let broadcast_close_vote = matches!(
            target.metadata.election_id,
            rsnano_types::RaiElectionId::CloseCut { .. }
                | rsnano_types::RaiElectionId::CloseRecord { .. }
        );
        let generated = std::sync::atomic::AtomicUsize::new(0);
        self.shared_state.vote(
            &[target.hash],
            &[target.root],
            std::slice::from_ref(&target.metadata),
            false,
            |vote| {
                generated.fetch_add(1, Ordering::Relaxed);
                let message = Message::ConfirmAck(ConfirmAck::new_with_own_vote((*vote).clone()));
                self.shared_state.message_sender.lock().unwrap().try_send(
                    channel,
                    &message,
                    if broadcast_close_vote {
                        TrafficType::RaiCloseControl
                    } else {
                        TrafficType::VoteReply
                    },
                );
                // Close-round liveness depends on every signed leaf being
                // disseminated as ordinary vote gossip. A point-to-point
                // solicitation reply alone can leave different replicas with
                // disjoint First-vote subsets, so none can derive either a
                // deciding value or the positive split/timeout death proof.
                if broadcast_close_vote {
                    self.shared_state.vote_broadcaster.broadcast_rai_close(vote);
                }
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
        let broadcast_close_vote = matches!(
            target.metadata.election_id,
            rsnano_types::RaiElectionId::CloseCut { .. }
                | rsnano_types::RaiElectionId::CloseRecord { .. }
        );
        let generated = std::sync::atomic::AtomicUsize::new(0);
        self.shared_state.vote(
            &[target.hash],
            &[target.root],
            std::slice::from_ref(&target.metadata),
            true,
            |vote| {
                generated.fetch_add(1, Ordering::Relaxed);
                let message = Message::ConfirmAck(ConfirmAck::new_with_own_vote((*vote).clone()));
                self.shared_state.message_sender.lock().unwrap().try_send(
                    channel,
                    &message,
                    if broadcast_close_vote {
                        TrafficType::RaiCloseControl
                    } else {
                        TrafficType::VoteReply
                    },
                );
                if broadcast_close_vote {
                    self.shared_state.vote_broadcaster.broadcast_rai_close(vote);
                }
            },
        );
        generated.load(Ordering::Relaxed)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn reply_cached_rai_election_votes(
        &self,
        root: &rsnano_types::Root,
        metadata: &RaiVoteMetadata,
        channel: &Arc<Channel>,
    ) -> usize {
        let votes = self
            .shared_state
            .history
            .rai_votes_for_election(root, &metadata.election_id);
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

    /// Replays retained, already-signed slot evidence when the AEC context has
    /// been pruned after close. No vote generation is permitted on this path.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn reply_cached_rai_slot_votes_for_roots(
        &self,
        roots: &[(Root, Option<rsnano_types::RaiElectionId>)],
        channel: &Arc<Channel>,
    ) -> usize {
        // This contextless fallback is reachable from the public ConfirmReq
        // surface. Bound both distinct root lookups and returned transports;
        // the lagging peer's periodic 16-root requests provide retry/fairness.
        const MAX_REPLAY_ONLY_ROOTS: usize = 16;
        const MAX_REPLAY_ONLY_TRANSPORTS: usize = 64;
        let mut seen_roots = HashSet::new();
        let mut sent = HashSet::new();
        for (root, excluded_election) in roots
            .iter()
            .filter(|item| seen_roots.insert((*item).clone()))
            .take(MAX_REPLAY_ONLY_ROOTS)
        {
            for vote in self
                .shared_state
                .history
                .rai_slot_votes_for_root(root, excluded_election.as_ref())
            {
                if !sent.insert((vote.voter, vote.signature.clone())) {
                    continue;
                }
                let confirm = Message::ConfirmAck(ConfirmAck::new_with_own_vote((*vote).clone()));
                if !self.shared_state.message_sender.lock().unwrap().try_send(
                    channel,
                    &confirm,
                    TrafficType::RaiRepairControl,
                ) || sent.len() >= MAX_REPLAY_ONLY_TRANSPORTS
                {
                    return sent.len();
                }
            }
        }
        sent.len()
    }

    /// Replies to one batched timeout repair request. A retained signed vote
    /// can cover several requested elections, so replay each transport once,
    /// then sign all still-missing current leaves together.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn reply_cached_and_generate_rai_slot_votes(
        &self,
        contexts: &[(Root, RaiVoteMetadata)],
        targets: &[rsnano_ledger::RaiFinalizedVoteTarget],
        channel: &Arc<Channel>,
    ) -> usize {
        let mut sent_cached = HashSet::new();
        for (root, metadata) in contexts {
            for vote in self
                .shared_state
                .history
                .rai_votes_for_election(root, &metadata.election_id)
            {
                // Local history indexes one signed transport under every
                // leaf it covers.  Use its validated signature identity here:
                // hashing the whole transport once per requested leaf makes
                // replay of a B-leaf batch O(B^2).
                if !sent_cached.insert((vote.voter, vote.signature.clone())) {
                    continue;
                }
                let confirm = Message::ConfirmAck(ConfirmAck::new_with_own_vote((*vote).clone()));
                self.shared_state.message_sender.lock().unwrap().try_send(
                    channel,
                    &confirm,
                    TrafficType::RaiRepairControl,
                );
            }
        }

        let generated = std::sync::atomic::AtomicUsize::new(0);
        for chunk in targets.chunks(Self::MAX_HASHES) {
            let hashes = chunk.iter().map(|target| target.hash).collect::<Vec<_>>();
            let roots = chunk.iter().map(|target| target.root).collect::<Vec<_>>();
            let metadata = chunk
                .iter()
                .map(|target| target.metadata.clone())
                .collect::<Vec<_>>();
            self.shared_state
                .vote(&hashes, &roots, &metadata, false, |vote| {
                    generated.fetch_add(1, Ordering::Relaxed);
                    let confirm =
                        Message::ConfirmAck(ConfirmAck::new_with_own_vote((*vote).clone()));
                    self.shared_state.message_sender.lock().unwrap().try_send(
                        channel,
                        &confirm,
                        TrafficType::RaiRepairControl,
                    );
                });
        }

        sent_cached.len() + generated.load(Ordering::Relaxed)
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
            #[cfg(feature = "rai_protocol")]
            spacing: Mutex::new(RaiVoteSpacing::new(voting_delay)),
            #[cfg(not(feature = "rai_protocol"))]
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
    ) -> bool {
        self.vote_generation_queue.try_add(VoteCandidate {
            root: *root,
            hash: *hash,
            #[cfg(feature = "rai_protocol")]
            metadata,
            #[cfg(feature = "rai_protocol")]
            is_rai_close,
        })
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
                        // ConfirmReq carries only the legacy (hash, root)
                        // pair, so the requester may be repairing any RAI
                        // epoch-qualified election for that pair. Replay all
                        // retained signed contexts even when this replica has
                        // a different local election active at the same root.
                        // Fresh signing below remains restricted to the local
                        // active context supplied by the AEC.
                        let votes = self.shared_state.history.rai_votes_for_candidate(
                            &i.root(),
                            &i.hash(),
                            None,
                        );
                        if !votes.is_empty() {
                            for vote in votes {
                                // Every signer is distinct certificate
                                // evidence, while the same signed transport
                                // may be indexed under every requested leaf.
                                // Signature identity avoids recomputing its
                                // full B-leaf hash B times.
                                if !sent_cached.insert((vote.voter, vote.signature.clone())) {
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
    root: &Root,
    hash: &BlockHash,
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
        if !history.rai_support_is_compatible(root, hash, voter, &metadata) {
            return None;
        }
        return Some(metadata);
    }

    let mut final_metadata = metadata.clone();
    final_metadata.phase = RaiVotePhase::Final;
    if history.rai_phase_vote_exists(root, &BlockHash::ZERO, voter, &final_metadata) {
        return None;
    }

    if metadata.phase == RaiVotePhase::Notar {
        let mut first_metadata = metadata.clone();
        first_metadata.phase = RaiVotePhase::First;
        if !history.rai_phase_vote_exists(root, &BlockHash::ZERO, voter, &first_metadata) {
            metadata.phase = RaiVotePhase::First;
        }
    }

    Some(metadata)
}

#[cfg(feature = "rai_protocol")]
type RaiSigningEntry = (Root, BlockHash, RaiVoteMetadata);

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum RaiWireBatchKind {
    BlockSlot,
    TimeoutSlot,
    Close,
}

#[cfg(feature = "rai_protocol")]
fn rai_wire_batch_kind(entry: &RaiSigningEntry) -> RaiWireBatchKind {
    match entry.2.election_id {
        RaiElectionId::Slot(_) if entry.1.is_zero() => RaiWireBatchKind::TimeoutSlot,
        RaiElectionId::Slot(_) => RaiWireBatchKind::BlockSlot,
        RaiElectionId::CloseCut { .. } | RaiElectionId::CloseRecord { .. } => {
            RaiWireBatchKind::Close
        }
    }
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum RaiFinalLock {
    Unlocked,
    SameValue,
    ConflictingValue,
}

#[cfg(feature = "rai_protocol")]
fn rai_scopes_overlap(first: RaiCommitteeScope, second: RaiCommitteeScope) -> bool {
    first == RaiCommitteeScope::All || second == RaiCommitteeScope::All || first == second
}

#[cfg(feature = "rai_protocol")]
fn rai_final_metadata_to_sign(
    history: &LocalVoteHistory,
    root: &Root,
    hash: &BlockHash,
    voter: &PublicKey,
    requested: &RaiVoteMetadata,
    regenerate_finalized: bool,
) -> Option<(RaiVoteMetadata, bool)> {
    let mut requested_final = requested.clone();
    requested_final.phase = RaiVotePhase::Final;
    let existing = history
        .rai_votes_for_election(root, &requested_final.election_id)
        .into_iter()
        .filter(|vote| vote.voter == *voter)
        .flat_map(|vote| {
            vote.rai_entries()
                .filter(|(metadata, _)| {
                    metadata.phase == RaiVotePhase::Final
                        && metadata.election_id == requested_final.election_id
                        && metadata.epoch == requested_final.epoch
                })
                .map(|(metadata, hash)| (metadata.scope, *hash))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let lock = |scope| {
        let mut same = false;
        for (current_scope, current_hash) in &existing {
            if !rai_scopes_overlap(*current_scope, scope) {
                continue;
            }
            if current_hash != hash {
                return RaiFinalLock::ConflictingValue;
            }
            same = true;
        }
        if same {
            RaiFinalLock::SameValue
        } else {
            RaiFinalLock::Unlocked
        }
    };

    let residual_scope = match requested_final.scope {
        RaiCommitteeScope::All => match (
            lock(RaiCommitteeScope::Older),
            lock(RaiCommitteeScope::Newer),
        ) {
            (RaiFinalLock::Unlocked, RaiFinalLock::Unlocked) => Some(RaiCommitteeScope::All),
            (RaiFinalLock::Unlocked, _) => Some(RaiCommitteeScope::Older),
            (_, RaiFinalLock::Unlocked) => Some(RaiCommitteeScope::Newer),
            _ => None,
        },
        scope => (lock(scope) == RaiFinalLock::Unlocked).then_some(scope),
    };

    if let Some(scope) = residual_scope {
        requested_final.scope = scope;
        if history.rai_support_is_compatible(root, hash, voter, &requested_final) {
            return Some((requested_final, false));
        }
        return None;
    }

    // Durable repair can reproduce an existing exact logical Final leaf, but
    // it cannot broaden a scoped lock or change its value. No companion First
    // is emitted on this path because the Final action already happened.
    let exact_same_leaf = existing
        .iter()
        .any(|(scope, current_hash)| *scope == requested_final.scope && current_hash == hash);
    let no_conflicting_lock = existing.iter().all(|(scope, current_hash)| {
        !rai_scopes_overlap(*scope, requested_final.scope) || current_hash == hash
    });
    (regenerate_finalized && exact_same_leaf && no_conflicting_lock)
        .then_some((requested_final, true))
}

#[cfg(feature = "rai_protocol")]
fn missing_rai_first_entries(
    history: &LocalVoteHistory,
    root: &Root,
    hash: &BlockHash,
    voter: &PublicKey,
    final_metadata: &RaiVoteMetadata,
) -> Vec<RaiSigningEntry> {
    let mut first = final_metadata.clone();
    first.phase = RaiVotePhase::First;

    let first_exists = |metadata: &RaiVoteMetadata| {
        history.rai_phase_vote_exists(root, &BlockHash::ZERO, voter, metadata)
    };

    match first.scope {
        // During an epoch overlap, an earlier scoped First may cover only one
        // half of an All-scoped Final. Fill exactly the disjoint remainder;
        // signing another All leaf would overlap the immutable earlier First.
        RaiCommitteeScope::All => {
            let mut older = first.clone();
            older.scope = RaiCommitteeScope::Older;
            let mut newer = first.clone();
            newer.scope = RaiCommitteeScope::Newer;
            match (first_exists(&older), first_exists(&newer)) {
                (false, false) => vec![(*root, *hash, first)],
                (true, false) => vec![(*root, *hash, newer)],
                (false, true) => vec![(*root, *hash, older)],
                (true, true) => Vec::new(),
            }
        }
        RaiCommitteeScope::Older | RaiCommitteeScope::Newer => {
            if first_exists(&first) {
                Vec::new()
            } else {
                vec![(*root, *hash, first)]
            }
        }
    }
}

#[cfg(feature = "rai_protocol")]
fn rai_signing_entry_groups(
    history: &LocalVoteHistory,
    roots: &[Root],
    hashes: &[BlockHash],
    requested: &[RaiVoteMetadata],
    voter: &PublicKey,
    is_final_generator: bool,
    regenerate_finalized: bool,
) -> Vec<Vec<RaiSigningEntry>> {
    debug_assert_eq!(roots.len(), hashes.len());
    debug_assert_eq!(roots.len(), requested.len());

    let mut groups = Vec::new();
    for ((root, hash), requested) in roots.iter().zip(hashes).zip(requested) {
        if is_final_generator {
            let Some((metadata, reproduces_locked_final)) = rai_final_metadata_to_sign(
                history,
                root,
                hash,
                voter,
                requested,
                regenerate_finalized,
            ) else {
                continue;
            };
            let mut group = Vec::with_capacity(2);
            if !reproduces_locked_final {
                group.extend(missing_rai_first_entries(
                    history, root, hash, voter, &metadata,
                ));
            }
            group.push((*root, *hash, metadata));
            groups.push(group);
            continue;
        }

        let Some(metadata) = rai_signing_metadata(
            history,
            root,
            hash,
            voter,
            requested.clone(),
            is_final_generator,
        ) else {
            continue;
        };

        if history.rai_phase_vote_exists(root, hash, voter, &metadata) {
            continue;
        }

        groups.push(vec![(*root, *hash, metadata)]);
    }
    groups
}

#[cfg(all(feature = "rai_protocol", test))]
fn rai_signing_entries(
    history: &LocalVoteHistory,
    roots: &[Root],
    hashes: &[BlockHash],
    requested: &[RaiVoteMetadata],
    voter: &PublicKey,
    is_final_generator: bool,
    regenerate_finalized: bool,
) -> Vec<RaiSigningEntry> {
    rai_signing_entry_groups(
        history,
        roots,
        hashes,
        requested,
        voter,
        is_final_generator,
        regenerate_finalized,
    )
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(feature = "rai_protocol")]
fn rai_signing_batches(
    history: &LocalVoteHistory,
    roots: &[Root],
    hashes: &[BlockHash],
    requested: &[RaiVoteMetadata],
    voter: &PublicKey,
    is_final_generator: bool,
    regenerate_finalized: bool,
) -> Vec<Vec<RaiSigningEntry>> {
    let groups = rai_signing_entry_groups(
        history,
        roots,
        hashes,
        requested,
        voter,
        is_final_generator,
        regenerate_finalized,
    );
    let mut batches = Vec::new();
    let mut batch: Vec<RaiSigningEntry> = Vec::with_capacity(VoteGenerator::MAX_HASHES);
    for group in groups {
        debug_assert!(group.len() <= VoteGenerator::MAX_HASHES);
        let changes_wire_kind = batch
            .first()
            .zip(group.first())
            .is_some_and(|(current, next)| {
                rai_wire_batch_kind(current) != rai_wire_batch_kind(next)
            });
        if !batch.is_empty()
            && (batch.len() + group.len() > VoteGenerator::MAX_HASHES || changes_wire_kind)
        {
            batches.push(std::mem::take(&mut batch));
            batch.reserve(VoteGenerator::MAX_HASHES);
        }
        batch.extend(group);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

#[cfg(feature = "rai_protocol")]
struct RaiVoteSpacing {
    delay: Duration,
    recent: HashMap<(RaiElectionId, RaiCommitteeScope), (BlockHash, Timestamp)>,
    expiry: VecDeque<(Timestamp, (RaiElectionId, RaiCommitteeScope))>,
}

#[cfg(feature = "rai_protocol")]
impl RaiVoteSpacing {
    fn new(delay: Duration) -> Self {
        Self {
            delay,
            recent: HashMap::new(),
            expiry: VecDeque::new(),
        }
    }

    fn votable(&self, metadata: &RaiVoteMetadata, hash: &BlockHash, now: Timestamp) -> bool {
        let context = (metadata.election_id.clone(), metadata.scope);
        self.recent
            .get(&context)
            .is_none_or(|(previous, timestamp)| previous == hash || now >= *timestamp + self.delay)
    }

    fn flag(&mut self, metadata: &RaiVoteMetadata, hash: &BlockHash, now: Timestamp) {
        self.trim(now);
        let context = (metadata.election_id.clone(), metadata.scope);
        self.recent.insert(context.clone(), (*hash, now));
        self.expiry.push_back((now, context));
    }

    fn trim(&mut self, now: Timestamp) {
        while let Some((timestamp, context)) = self.expiry.front() {
            if now < *timestamp + self.delay {
                break;
            }
            let timestamp = *timestamp;
            let context = context.clone();
            self.expiry.pop_front();
            if self
                .recent
                .get(&context)
                .is_some_and(|(_, current)| *current == timestamp)
            {
                self.recent.remove(&context);
            }
        }
    }
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
    #[cfg(feature = "rai_protocol")]
    spacing: Mutex<RaiVoteSpacing>,
    #[cfg(not(feature = "rai_protocol"))]
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
        let mut metadata = Vec::with_capacity(VoteGenerator::MAX_HASHES);
        {
            let spacing = self.spacing.lock().unwrap();
            while queues.candidates.front().is_some() {
                let candidate = queues.candidates.pop_front().unwrap();
                let root = candidate.root;
                let hash = candidate.hash;
                #[cfg(feature = "rai_protocol")]
                let new_context = !metadata.iter().any(|current: &RaiVoteMetadata| {
                    same_rai_vote_context(current, &candidate.metadata)
                });
                #[cfg(not(feature = "rai_protocol"))]
                let new_context = !roots.contains(&root);
                if new_context {
                    #[cfg(feature = "rai_protocol")]
                    let votable = spacing.votable(&candidate.metadata, &hash, self.clock.now());
                    #[cfg(not(feature = "rai_protocol"))]
                    let votable = spacing.votable(&root, &hash, self.clock.now());
                    if votable {
                        roots.push(root);
                        hashes.push(hash);
                        #[cfg(feature = "rai_protocol")]
                        metadata.push(candidate.metadata);
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
                &metadata,
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
        #[cfg(feature = "rai_protocol")] metadata: &[RaiVoteMetadata],
        #[cfg(feature = "rai_protocol")] regenerate_finalized: bool,
        action: F,
    ) where
        F: Fn(Arc<Vote>),
    {
        debug_assert_eq!(hashes.len(), roots.len());
        #[cfg(feature = "rai_protocol")]
        {
            debug_assert_eq!(metadata.len(), hashes.len());
            let mut rep_keys = self.rai_rep_keys();
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
                let voter = rep_key.public_key();
                let signed_batches = rai_signing_batches(
                    &self.history,
                    roots,
                    hashes,
                    metadata,
                    &voter,
                    self.is_final,
                    regenerate_finalized,
                );

                for signed_entries in signed_batches {
                    let is_slot_timeout = signed_entries.iter().all(|(_, hash, metadata)| {
                        hash.is_zero() && matches!(metadata.election_id, RaiElectionId::Slot(_))
                    });
                    let timeout_entries = is_slot_timeout.then(|| {
                        signed_entries
                            .iter()
                            .map(|(_, _, metadata)| {
                                let RaiElectionId::Slot(slot) = &metadata.election_id else {
                                    return None;
                                };
                                let locator = if slot.root.previous.is_zero() {
                                    RaiTimeoutSlot {
                                        account: Account::from(slot.root.root),
                                        height: 1,
                                    }
                                } else {
                                    let predecessor =
                                        self.ledger.any().get_block(&slot.root.previous)?;
                                    RaiTimeoutSlot {
                                        account: predecessor.account(),
                                        height: predecessor.height().saturating_add(1),
                                    }
                                };
                                Some((metadata.clone(), locator))
                            })
                            .collect::<Option<Vec<_>>>()
                    });
                    let vote = Arc::new(if is_slot_timeout {
                        let Some(Some(timeout_entries)) = timeout_entries else {
                            continue;
                        };
                        Vote::new_rai_timeout_batch(&rep_key, timestamp, duration, timeout_entries)
                    } else {
                        Vote::new_rai_batch(
                            &rep_key,
                            timestamp,
                            duration,
                            signed_entries
                                .iter()
                                .map(|(_, hash, metadata)| (metadata.clone(), *hash)),
                        )
                    });
                    votes.push((vote, signed_entries));
                }
            }

            // Phase selection and history insertion are one atomic local
            // signing decision shared by the final and non-final generators.
            for (vote, entries) in &votes {
                let now = self.clock.now();
                let mut spacing = self.spacing.lock().unwrap();
                for (root, hash, metadata) in entries {
                    self.history.add_rai(root, hash, metadata, vote);
                    spacing.flag(metadata, hash, now);
                }
            }
            drop(rai_signing_guard);

            for (vote, _) in votes {
                action(vote);
            }
        }

        #[cfg(not(feature = "rai_protocol"))]
        {
            let mut rep_keys = {
                let mut keys = Vec::new();
                self.wallet_reps.lock().unwrap().rep_priv_keys(&mut keys);
                keys
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
                votes.push(Arc::new(Vote::new(
                    &rep_key,
                    timestamp,
                    duration,
                    hashes.to_vec(),
                )));
            }

            for vote in votes {
                let now = self.clock.now();
                let mut spacing = self.spacing.lock().unwrap();
                for (root, hash) in roots.iter().zip(hashes) {
                    self.history.add(root, hash, &vote);
                    spacing.flag(root, hash, now);
                }
                drop(spacing);
                action(vote);
            }
        }
    }

    fn reply(&self, request: VoteRequest) {
        let mut i = request.candidates.iter().peekable();
        while i.peek().is_some() && !self.stopped.load(Ordering::SeqCst) {
            let mut hashes = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            let mut roots = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            #[cfg(feature = "rai_protocol")]
            let mut metadata = Vec::with_capacity(VoteGenerator::MAX_HASHES);
            {
                let spacing = self.spacing.lock().unwrap();
                while hashes.len() < VoteGenerator::MAX_HASHES {
                    let Some(candidate) = i.next() else {
                        break;
                    };
                    #[cfg(feature = "rai_protocol")]
                    let new_context = !metadata.iter().any(|current: &RaiVoteMetadata| {
                        same_rai_vote_context(current, &candidate.metadata)
                    });
                    #[cfg(not(feature = "rai_protocol"))]
                    let new_context = !roots.contains(&candidate.root);
                    if new_context {
                        #[cfg(feature = "rai_protocol")]
                        let votable =
                            spacing.votable(&candidate.metadata, &candidate.hash, self.clock.now());
                        #[cfg(not(feature = "rai_protocol"))]
                        let votable =
                            spacing.votable(&candidate.root, &candidate.hash, self.clock.now());
                        if votable {
                            roots.push(candidate.root);
                            hashes.push(candidate.hash);
                            #[cfg(feature = "rai_protocol")]
                            metadata.push(candidate.metadata.clone());
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
                    &metadata,
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
    use crate::consensus::{
        ActiveElectionsConfig, ActiveElectionsContainer, AecInsertRequest,
        election::ElectionBehavior,
    };
    use rsnano_ledger::{RepWeights, test_helpers::UnsavedBlockLatticeBuilder};
    use rsnano_types::{
        Amount, BlockPriority, PrivateKey, QualifiedRoot, RaiCommitteeScope, RaiElectionId,
        RaiEpoch, RaiSlotId,
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
            hash,
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
                &root,
                &hash,
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
                &root,
                &hash,
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
                &root,
                &hash,
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
                &root,
                &hash,
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
    fn batch_resolves_phase_and_duplicate_suppression_per_leaf() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let roots = [Root::from(1), Root::from(2)];
        let hashes = [BlockHash::from(11), BlockHash::from(12)];
        let requested = [
            metadata(roots[0], RaiVotePhase::Notar),
            metadata(roots[1], RaiVotePhase::Notar),
        ];
        remember(
            &history,
            &signer,
            roots[0],
            hashes[0],
            metadata(roots[0], RaiVotePhase::First),
        );

        let entries = rai_signing_entries(
            &history,
            &roots,
            &hashes,
            &requested,
            &signer.public_key(),
            false,
            false,
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].2.phase, RaiVotePhase::Notar);
        assert_eq!(entries[1].2.phase, RaiVotePhase::First);

        let vote = Vote::new_rai_batch(
            &signer,
            UnixMillisTimestamp::new(16),
            0,
            entries
                .iter()
                .map(|(_, hash, metadata)| (metadata.clone(), *hash)),
        );
        assert_eq!(vote.rai_entry_count(), 2);

        remember(
            &history,
            &signer,
            roots[0],
            hashes[0],
            metadata(roots[0], RaiVotePhase::Notar),
        );
        let remaining = rai_signing_entries(
            &history,
            &roots,
            &hashes,
            &requested,
            &signer.public_key(),
            false,
            false,
        );
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, roots[1]);
        assert_eq!(remaining[0].2.phase, RaiVotePhase::First);
    }

    #[test]
    fn batch_separates_compact_slot_and_close_wire_formats() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let roots = [Root::from(1), Root::from(2)];
        let hashes = [BlockHash::from(11), BlockHash::from(12)];
        let slot = metadata(roots[0], RaiVotePhase::First);
        let close = RaiVoteMetadata {
            election_id: RaiElectionId::CloseRecord {
                epoch: RaiEpoch::ZERO,
                round: 0,
            },
            phase: RaiVotePhase::First,
            epoch: RaiEpoch::ZERO,
            scope: RaiCommitteeScope::All,
        };

        let batches = rai_signing_batches(
            &history,
            &roots,
            &hashes,
            &[slot, close],
            &signer.public_key(),
            false,
            false,
        );

        assert_eq!(batches.len(), 2);
        for batch in batches {
            let vote = Vote::new_rai_batch(
                &signer,
                UnixMillisTimestamp::new(16),
                0,
                batch
                    .iter()
                    .map(|(_, hash, metadata)| (metadata.clone(), *hash)),
            );
            let mut bytes = Vec::new();
            vote.serialize(&mut bytes).unwrap();
        }
    }

    #[test]
    fn batch_omits_only_the_leaf_with_conflicting_final_support() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let roots = [Root::from(1), Root::from(2)];
        let hashes = [BlockHash::from(11), BlockHash::from(12)];
        let requested = [
            metadata(roots[0], RaiVotePhase::Notar),
            metadata(roots[1], RaiVotePhase::Notar),
        ];
        remember(
            &history,
            &signer,
            roots[0],
            BlockHash::ZERO,
            metadata(roots[0], RaiVotePhase::First),
        );

        let entries = rai_signing_entries(
            &history,
            &roots,
            &hashes,
            &requested,
            &signer.public_key(),
            true,
            false,
        );
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.0 == roots[1]));
        assert_eq!(entries[0].2.phase, RaiVotePhase::First);
        assert_eq!(entries[1].2.phase, RaiVotePhase::Final);
    }

    #[test]
    fn disjoint_committee_scopes_are_independent_batch_leaves() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let root = Root::from(1);
        let hashes = [BlockHash::from(11), BlockHash::from(12)];
        let mut older = metadata(root, RaiVotePhase::First);
        older.scope = RaiCommitteeScope::Older;
        let mut newer = older.clone();
        newer.scope = RaiCommitteeScope::Newer;
        assert!(!same_rai_vote_context(&older, &newer));

        let initial = rai_signing_entries(
            &history,
            &[root, root],
            &hashes,
            &[older.clone(), newer.clone()],
            &signer.public_key(),
            false,
            false,
        );
        assert_eq!(initial.len(), 2);

        remember(&history, &signer, root, hashes[0], older.clone());
        let entries = rai_signing_entries(
            &history,
            &[root, root],
            &hashes,
            &[older, newer.clone()],
            &signer.public_key(),
            false,
            false,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].2.scope, RaiCommitteeScope::Newer);
    }

    #[test]
    fn rai_spacing_is_qualified_by_election_and_scope() {
        let now = Timestamp::new_test_instance();
        let delay = Duration::from_millis(100);
        let mut spacing = RaiVoteSpacing::new(delay);
        let root = Root::from(1);
        let first_hash = BlockHash::from(11);
        let other_hash = BlockHash::from(12);
        let mut older = metadata(root, RaiVotePhase::First);
        older.scope = RaiCommitteeScope::Older;
        let mut newer = older.clone();
        newer.scope = RaiCommitteeScope::Newer;
        let other_election = metadata(Root::from(2), RaiVotePhase::First);

        spacing.flag(&older, &first_hash, now);

        assert!(spacing.votable(&older, &first_hash, now));
        assert!(!spacing.votable(&older, &other_hash, now));
        assert!(spacing.votable(&newer, &other_hash, now));
        assert!(spacing.votable(&other_election, &other_hash, now));
        assert!(spacing.votable(&older, &other_hash, now + delay));
    }

    #[test]
    fn cached_large_batch_is_replied_once_per_request() {
        // Keep this large enough to exercise the shared-transport path: the
        // reply count must stay constant as the number of indexed leaves grows.
        let blocks = (10..74)
            .map(SavedBlock::new_test_instance_with_key)
            .collect::<Vec<_>>();
        let contexts = blocks
            .iter()
            .map(|block| (metadata(block.root(), RaiVotePhase::First), false))
            .collect::<Vec<_>>();
        let history = Arc::new(LocalVoteHistory::with_max_cache(128));
        let signer = PrivateKey::new();
        let vote = Arc::new(Vote::new_rai_batch(
            &signer,
            UnixMillisTimestamp::new(16),
            0,
            blocks
                .iter()
                .zip(&contexts)
                .map(|(block, (metadata, _))| (metadata.clone(), block.hash())),
        ));
        for (block, (metadata, _)) in blocks.iter().zip(&contexts) {
            history.add_rai(&block.root(), &block.hash(), metadata, &vote);
        }

        let message_sender = MessageSender::new_null();
        let sent = message_sender.track();
        let generator = VoteGenerator::new(
            Arc::new(Ledger::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            history,
            Arc::new(Mutex::new(())),
            false,
            Arc::new(Stats::default()),
            message_sender,
            Duration::from_millis(100),
            Duration::from_millis(10),
            Arc::new(VoteBroadcaster::new_null()),
            Arc::new(SteadyClock::new_null()),
        );
        let channel = Arc::new(Channel::new_test_instance());

        generator.generate(&blocks, &channel, &contexts);

        let sent = sent.output();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].traffic_type, TrafficType::Vote);
        let Message::ConfirmAck(confirm_ack) = &sent[0].message else {
            panic!("expected cached ConfirmAck");
        };
        assert_eq!(confirm_ack.vote().hash(), vote.hash());
        assert_eq!(confirm_ack.vote().rai_entry_count(), blocks.len());
    }

    #[test]
    fn legacy_confirm_req_replays_older_epoch_while_successor_is_active() {
        let signer = PrivateKey::new();
        let committee = Arc::new(RepWeights::from([(signer.public_key(), Amount::raw(100))]));
        let now = Timestamp::new_test_instance();
        let epoch_duration = Duration::from_secs(30);
        let mut active = ActiveElectionsContainer::new_with_rai_committee(
            ActiveElectionsConfig::default(),
            Duration::from_millis(100),
            committee,
            BlockHash::from(7),
        );
        active.rai_set_open_started_at(now);
        active.rai_tick(now + epoch_duration, &signer, epoch_duration);
        assert_eq!(active.rai_epoch_state().open_epoch, RaiEpoch::new(1));

        let ledger = Arc::new(Ledger::new_null());
        let block = UnsavedBlockLatticeBuilder::new()
            .genesis()
            .send(100, Amount::raw(1));
        let block = ledger.process_one(&block).unwrap();
        active
            .insert(
                AecInsertRequest {
                    block: block.clone(),
                    behavior: ElectionBehavior::Priority,
                    priority: BlockPriority::default(),
                },
                now + epoch_duration,
            )
            .unwrap();
        let root = block.root();
        let hash = block.hash();
        let (successor, is_close) = active.rai_vote_context(&hash).unwrap();
        assert_eq!(successor.epoch, RaiEpoch::new(1));
        assert!(!is_close);

        let mut older = metadata(root, RaiVotePhase::First);
        let qualified_root = block.qualified_root();
        older.epoch = RaiEpoch::ZERO;
        older.election_id = RaiElectionId::Slot(RaiSlotId {
            epoch: older.epoch,
            root: qualified_root.clone(),
        });
        let mut older_notar = older.clone();
        older_notar.phase = RaiVotePhase::Notar;
        let older_first_vote = Arc::new(Vote::new_rai(
            &signer,
            UnixMillisTimestamp::new(16),
            0,
            hash,
            older.clone(),
        ));
        let older_notar_vote = Arc::new(Vote::new_rai(
            &signer,
            UnixMillisTimestamp::new(32),
            0,
            hash,
            older_notar.clone(),
        ));
        let history = Arc::new(LocalVoteHistory::with_max_cache(8));
        history.add_rai(&root, &hash, &older, &older_first_vote);
        history.add_rai(&root, &hash, &older_notar, &older_notar_vote);

        let message_sender = MessageSender::new_null();
        let sent = message_sender.track();
        let generator = VoteGenerator::new(
            ledger,
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            history,
            Arc::new(Mutex::new(())),
            false,
            Arc::new(Stats::default()),
            message_sender,
            Duration::from_millis(100),
            Duration::from_millis(10),
            Arc::new(VoteBroadcaster::new_null()),
            Arc::new(SteadyClock::new_null()),
        );
        generator
            .shared_state
            .rai_rep_keys
            .lock()
            .unwrap()
            .push(signer);
        let channel = Arc::new(Channel::new_test_instance());

        // The responder's local AEC context is epoch 1. Because legacy
        // ConfirmReq did not carry that context, both epoch-0 certificate
        // transports must still be returned to a peer draining the old cut.
        // Only the fresh response is allowed to use the local epoch-1 context.
        assert_eq!(
            generator.generate(&[block], &channel, &[(successor.clone(), false)]),
            1
        );
        let fresh_request = generator
            .shared_state
            .queues
            .lock()
            .unwrap()
            .requests
            .pop_front()
            .unwrap();
        generator.shared_state.reply(fresh_request);

        let sent = sent.output();
        assert_eq!(sent.len(), 3);
        let replayed = sent
            .iter()
            .map(|event| match &event.message {
                Message::ConfirmAck(confirm_ack) => {
                    confirm_ack.vote().rai_metadata(0).cloned().unwrap()
                }
                _ => panic!("expected cached ConfirmAck"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            replayed
                .iter()
                .filter(|metadata| metadata.epoch == RaiEpoch::ZERO)
                .map(|metadata| metadata.phase)
                .collect::<HashSet<_>>(),
            HashSet::from([RaiVotePhase::First, RaiVotePhase::Notar])
        );
        assert_eq!(
            replayed
                .iter()
                .filter(|metadata| metadata.epoch == RaiEpoch::new(1))
                .collect::<Vec<_>>(),
            vec![&successor]
        );
    }

    #[test]
    fn timeout_request_replays_one_cached_batch_and_generates_one_fresh_batch() {
        let roots = (1..65).map(Root::from).collect::<Vec<_>>();
        let epoch = RaiEpoch::new(7);
        let timeout_metadata = |root, phase| RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch,
                root: QualifiedRoot::new(root, BlockHash::ZERO),
            }),
            phase,
            epoch,
            scope: RaiCommitteeScope::All,
        };
        let cached_metadata = roots
            .iter()
            .map(|root| timeout_metadata(*root, RaiVotePhase::First))
            .collect::<Vec<_>>();
        let current_metadata = roots
            .iter()
            .map(|root| timeout_metadata(*root, RaiVotePhase::Notar))
            .collect::<Vec<_>>();
        let history = Arc::new(LocalVoteHistory::with_max_cache(128));
        let signer = PrivateKey::new();
        let cached_vote = Arc::new(Vote::new_rai_batch(
            &signer,
            UnixMillisTimestamp::new(16),
            0,
            cached_metadata
                .iter()
                .map(|metadata| (metadata.clone(), BlockHash::ZERO)),
        ));
        for (root, metadata) in roots.iter().zip(&cached_metadata) {
            history.add_rai(root, &BlockHash::ZERO, metadata, &cached_vote);
        }

        let message_sender = MessageSender::new_null();
        let sent = message_sender.track();
        let generator = VoteGenerator::new(
            Arc::new(Ledger::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            history,
            Arc::new(Mutex::new(())),
            false,
            Arc::new(Stats::default()),
            message_sender,
            Duration::from_millis(100),
            Duration::from_millis(10),
            Arc::new(VoteBroadcaster::new_null()),
            Arc::new(SteadyClock::new_null()),
        );
        generator
            .shared_state
            .rai_rep_keys
            .lock()
            .unwrap()
            .push(signer);
        let contexts = roots
            .iter()
            .zip(&current_metadata)
            .map(|(root, metadata)| (*root, metadata.clone()))
            .collect::<Vec<_>>();
        let targets = roots
            .iter()
            .zip(&current_metadata)
            .map(|(root, metadata)| rsnano_ledger::RaiFinalizedVoteTarget {
                election_id: metadata.election_id.clone(),
                hash: BlockHash::ZERO,
                root: *root,
                metadata: metadata.clone(),
            })
            .collect::<Vec<_>>();
        let channel = Arc::new(Channel::new_test_instance());

        assert_eq!(
            generator.reply_cached_and_generate_rai_slot_votes(&contexts, &targets, &channel),
            2
        );

        let sent = sent.output();
        assert_eq!(sent.len(), 2);
        assert!(
            sent.iter()
                .all(|event| event.traffic_type == TrafficType::RaiRepairControl)
        );
        let votes = sent
            .iter()
            .map(|event| match &event.message {
                Message::ConfirmAck(confirm_ack) => confirm_ack.vote(),
                _ => panic!("expected ConfirmAck"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            votes
                .iter()
                .filter(|vote| vote.hash() == cached_vote.hash())
                .count(),
            1
        );
        let generated = votes
            .iter()
            .find(|vote| vote.hash() != cached_vote.hash())
            .unwrap();
        assert_eq!(generated.rai_entry_count(), roots.len());
        assert!(generated.hashes.iter().all(BlockHash::is_zero));
        assert!(generated.is_rai_timeout_slot_batch());
        assert!(roots.iter().enumerate().all(|(index, root)| {
            generated.rai_timeout_slot(index)
                == Some(RaiTimeoutSlot {
                    account: Account::from(*root),
                    height: 1,
                })
        }));
        assert!(
            generated.metadata.iter().all(|metadata| {
                metadata.phase == RaiVotePhase::Notar && metadata.epoch == epoch
            })
        );
        assert_eq!(
            generated
                .metadata
                .iter()
                .map(|metadata| metadata.election_id.clone())
                .collect::<HashSet<_>>(),
            current_metadata
                .iter()
                .map(|metadata| metadata.election_id.clone())
                .collect::<HashSet<_>>()
        );
    }

    #[test]
    fn final_action_pairs_a_missing_first_with_final() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let requested = metadata(root, RaiVotePhase::Notar);

        let batches = rai_signing_batches(
            &history,
            &[root],
            &[hash],
            std::slice::from_ref(&requested),
            &signer.public_key(),
            true,
            false,
        );
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 2);
        assert_eq!(batches[0][0].2.phase, RaiVotePhase::First);
        assert_eq!(batches[0][1].2.phase, RaiVotePhase::Final);

        let vote = Vote::new_rai_batch(
            &signer,
            Vote::TIMESTAMP_MAX,
            Vote::DURATION_MAX,
            batches[0]
                .iter()
                .map(|(_, hash, metadata)| (metadata.clone(), *hash)),
        );
        assert_eq!(
            vote.metadata
                .iter()
                .map(|metadata| metadata.phase)
                .collect::<Vec<_>>(),
            vec![RaiVotePhase::First, RaiVotePhase::Final]
        );
    }

    #[test]
    fn final_vote_indexes_companion_first_in_the_same_transport_before_send() {
        let history = Arc::new(LocalVoteHistory::with_max_cache(32));
        let signer = PrivateKey::new();
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let requested = metadata(root, RaiVotePhase::Notar);
        let generator = VoteGenerator::new(
            Arc::new(Ledger::new_null()),
            Arc::new(Mutex::new(WalletRepresentatives::new_null())),
            Arc::clone(&history),
            Arc::new(Mutex::new(())),
            true,
            Arc::new(Stats::default()),
            MessageSender::new_null(),
            Duration::from_millis(100),
            Duration::from_millis(10),
            Arc::new(VoteBroadcaster::new_null()),
            Arc::new(SteadyClock::new_null()),
        );
        generator
            .shared_state
            .rai_rep_keys
            .lock()
            .unwrap()
            .push(signer.clone());
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sent_l = Arc::clone(&sent);
        let history_l = Arc::clone(&history);
        let voter = signer.public_key();
        let mut first = requested.clone();
        first.phase = RaiVotePhase::First;
        let mut final_vote = requested.clone();
        final_vote.phase = RaiVotePhase::Final;

        generator.shared_state.vote(
            &[hash],
            &[root],
            std::slice::from_ref(&requested),
            false,
            |vote| {
                assert!(history_l.rai_phase_vote_exists(&root, &hash, &voter, &first));
                assert!(history_l.rai_phase_vote_exists(&root, &hash, &voter, &final_vote));
                sent_l.lock().unwrap().push(vote);
            },
        );

        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].rai_entry_count(), 2);
        let retained = history.rai_votes_for_election(&root, &requested.election_id);
        assert_eq!(retained.len(), 1);
        assert!(Arc::ptr_eq(&retained[0], &sent[0]));
    }

    #[test]
    fn final_action_with_prior_first_emits_only_final() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let requested = metadata(root, RaiVotePhase::Notar);

        remember(
            &history,
            &signer,
            root,
            hash,
            metadata(root, RaiVotePhase::First),
        );
        let batches = rai_signing_batches(
            &history,
            &[root],
            &[hash],
            std::slice::from_ref(&requested),
            &signer.public_key(),
            true,
            false,
        );
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1);
        assert_eq!(batches[0][0].2.phase, RaiVotePhase::Final);
    }

    #[test]
    fn cached_final_never_authorizes_a_later_first() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let root = Root::from(1);
        let hash = BlockHash::from(2);
        let requested = metadata(root, RaiVotePhase::Notar);

        remember(
            &history,
            &signer,
            root,
            hash,
            metadata(root, RaiVotePhase::Final),
        );
        assert!(
            rai_signing_batches(
                &history,
                &[root],
                &[hash],
                std::slice::from_ref(&requested),
                &signer.public_key(),
                false,
                false,
            )
            .is_empty()
        );

        // A durable-target Final repair may refresh the Final transport, but
        // it must not manufacture a post-lock First alongside that replay.
        let repair = rai_signing_batches(
            &history,
            &[root],
            &[hash],
            std::slice::from_ref(&requested),
            &signer.public_key(),
            true,
            true,
        );
        assert_eq!(repair.len(), 1);
        assert_eq!(repair[0].len(), 1);
        assert_eq!(repair[0][0].2.phase, RaiVotePhase::Final);
    }

    #[test]
    fn all_scoped_final_fills_only_the_missing_first_scope() {
        for (existing_scope, missing_scope) in [
            (RaiCommitteeScope::Older, RaiCommitteeScope::Newer),
            (RaiCommitteeScope::Newer, RaiCommitteeScope::Older),
        ] {
            let history = LocalVoteHistory::with_max_cache(32);
            let signer = PrivateKey::new();
            let root = Root::from(1);
            let hash = BlockHash::from(2);
            let requested = metadata(root, RaiVotePhase::Notar);
            let mut existing_first = metadata(root, RaiVotePhase::First);
            existing_first.scope = existing_scope;
            remember(&history, &signer, root, hash, existing_first);

            let batches = rai_signing_batches(
                &history,
                &[root],
                &[hash],
                std::slice::from_ref(&requested),
                &signer.public_key(),
                true,
                false,
            );
            assert_eq!(batches.len(), 1);
            assert_eq!(batches[0].len(), 2);
            assert_eq!(batches[0][0].2.phase, RaiVotePhase::First);
            assert_eq!(batches[0][0].2.scope, missing_scope);
            assert_eq!(batches[0][1].2.phase, RaiVotePhase::Final);
            assert_eq!(batches[0][1].2.scope, RaiCommitteeScope::All);
        }
    }

    #[test]
    fn all_scoped_final_subtracts_an_existing_scoped_final_lock() {
        for (existing_scope, residual_scope) in [
            (RaiCommitteeScope::Older, RaiCommitteeScope::Newer),
            (RaiCommitteeScope::Newer, RaiCommitteeScope::Older),
        ] {
            let history = LocalVoteHistory::with_max_cache(32);
            let signer = PrivateKey::new();
            let root = Root::from(1);
            let hash = BlockHash::from(2);
            let requested = metadata(root, RaiVotePhase::Notar);
            let mut existing_final = metadata(root, RaiVotePhase::Final);
            existing_final.scope = existing_scope;
            remember(&history, &signer, root, hash, existing_final);

            let batches = rai_signing_batches(
                &history,
                &[root],
                &[hash],
                std::slice::from_ref(&requested),
                &signer.public_key(),
                true,
                false,
            );
            assert_eq!(batches.len(), 1);
            assert_eq!(batches[0].len(), 2);
            assert_eq!(batches[0][0].2.phase, RaiVotePhase::First);
            assert_eq!(batches[0][0].2.scope, residual_scope);
            assert_eq!(batches[0][1].2.phase, RaiVotePhase::Final);
            assert_eq!(batches[0][1].2.scope, residual_scope);
        }
    }

    #[test]
    fn final_repair_rejects_a_conflicting_existing_final_lock() {
        let history = LocalVoteHistory::with_max_cache(32);
        let signer = PrivateKey::new();
        let root = Root::from(1);
        let requested_hash = BlockHash::from(2);
        let requested = metadata(root, RaiVotePhase::Notar);
        remember(
            &history,
            &signer,
            root,
            BlockHash::from(3),
            metadata(root, RaiVotePhase::Final),
        );

        assert!(
            rai_signing_batches(
                &history,
                &[root],
                &[requested_hash],
                &[requested],
                &signer.public_key(),
                true,
                true,
            )
            .is_empty()
        );
    }

    #[test]
    fn expanded_final_entries_are_chunked_without_splitting_pairs() {
        let history = LocalVoteHistory::with_max_cache(1024);
        let signer = PrivateKey::new();
        let roots = (1..=VoteGenerator::MAX_HASHES)
            .map(|value| Root::from(value as u64))
            .collect::<Vec<_>>();
        let hashes = (1..=VoteGenerator::MAX_HASHES)
            .map(|value| BlockHash::from((value + 1000) as u64))
            .collect::<Vec<_>>();
        let requested = roots
            .iter()
            .map(|root| metadata(*root, RaiVotePhase::Notar))
            .collect::<Vec<_>>();

        let batches = rai_signing_batches(
            &history,
            &roots,
            &hashes,
            &requested,
            &signer.public_key(),
            true,
            false,
        );
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), roots.len() * 2);
        assert!(
            batches
                .iter()
                .all(|batch| batch.len() <= VoteGenerator::MAX_HASHES)
        );

        let mut flattened = Vec::new();
        for batch in &batches {
            assert_eq!(batch.len() % 2, 0);
            for pair in batch.chunks_exact(2) {
                assert_eq!(pair[0].0, pair[1].0);
                assert_eq!(pair[0].1, pair[1].1);
                assert_eq!(pair[0].2.phase, RaiVotePhase::First);
                assert_eq!(pair[1].2.phase, RaiVotePhase::Final);
            }
            let vote = Vote::new_rai_batch(
                &signer,
                Vote::TIMESTAMP_MAX,
                Vote::DURATION_MAX,
                batch
                    .iter()
                    .map(|(_, hash, metadata)| (metadata.clone(), *hash)),
            );
            assert_eq!(vote.rai_entry_count(), batch.len());
            flattened.extend(batch.iter().map(|(root, _, _)| *root));
        }
        assert_eq!(
            flattened,
            roots
                .iter()
                .flat_map(|root| [*root, *root])
                .collect::<Vec<_>>()
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
            rai_signing_batches(
                &history,
                &[root],
                &[hash],
                &[requested],
                &signer.public_key(),
                true,
                false,
            )
            .is_empty()
        );
    }
}
