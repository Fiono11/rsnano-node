use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_messages::{ConfirmAck, Message, Publish};
use rsnano_network::{Channel, ChannelEvent, ChannelId, TrafficType};
use rsnano_types::{Block, BlockHash, ConsensusEpoch, Root, Vote, VoteKind};
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    fair_queue::FairQueue,
    stats::{DetailType, Direction, StatType, Stats},
};

use super::{
    VoteGenerators,
    request_aggregator_impl::{AggregateResult, RequestAggregatorImpl, search_for_block},
};
use crate::{
    consensus::{
        AecService,
        election::{ElectionId, VoteType},
    },
    transport::MessageSender,
};

#[derive(Clone, Debug, PartialEq)]
pub struct RequestAggregatorConfig {
    pub threads: usize,
    pub max_queue: usize,
    pub batch_size: usize,
}

impl RequestAggregatorConfig {
    pub fn new(parallelism: usize) -> Self {
        Self {
            threads: (parallelism / 2).clamp(1, 4),
            max_queue: 128,
            batch_size: 16,
        }
    }
}

///  Pools together confirmation requests, separately for each endpoint.
///  Requests are added from network messages, and aggregated to minimize bandwidth and vote generation. Example:
///  * Two votes are cached, one for hashes {1,2,3} and another for hashes {4,5,6}
///  * A request arrives for hashes {1,4,5}. Another request arrives soon afterwards for hashes {2,3,6}
///  * The aggregator will reply with the two cached votes
///
///  Votes are generated for uncached hashes.
pub struct RequestAggregator {
    config: RequestAggregatorConfig,
    stats: Arc<Stats>,
    vote_generators: Arc<VoteGenerators>,
    ledger: Arc<Ledger>,
    active_elections: Arc<AecService>,
    message_sender: MessageSender,
    evidence_replies: Arc<Mutex<EvidenceReplyCache>>,
    state: Arc<Mutex<RequestAggregatorState>>,
    condition: Arc<Condvar>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl RequestAggregator {
    pub fn new(
        config: RequestAggregatorConfig,
        stats: Arc<Stats>,
        vote_generators: Arc<VoteGenerators>,
        ledger: Arc<Ledger>,
        active_elections: Arc<AecService>,
        message_sender: MessageSender,
    ) -> Self {
        let max_queue = config.max_queue;
        Self {
            stats,
            vote_generators,
            ledger,
            active_elections,
            message_sender,
            evidence_replies: Arc::new(Mutex::new(EvidenceReplyCache::default())),
            config,
            condition: Arc::new(Condvar::new()),
            state: Arc::new(Mutex::new(RequestAggregatorState {
                queue: FairQueue::new(move |_| max_queue, |_| 1),
                stopped: false,
            })),
            threads: Mutex::new(Vec::new()),
        }
    }

    pub fn new_null() -> Self {
        Self::new(
            RequestAggregatorConfig::new(1),
            Stats::default().into(),
            VoteGenerators::new_null().into(),
            Ledger::new_null().into(),
            AecService::new_null().into(),
            MessageSender::new_null(),
        )
    }

    pub fn start(&self) {
        let mut guard = self.threads.lock().unwrap();
        for _ in 0..self.config.threads {
            let aggregator_loop = RequestAggregatorLoop {
                mutex: self.state.clone(),
                condition: self.condition.clone(),
                stats: self.stats.clone(),
                config: self.config.clone(),
                ledger: self.ledger.clone(),
                vote_generators: self.vote_generators.clone(),
                active_elections: self.active_elections.clone(),
                message_sender: Mutex::new(self.message_sender.clone()),
                evidence_replies: self.evidence_replies.clone(),
            };

            guard.push(
                std::thread::Builder::new()
                    .name("Req aggregator".to_string())
                    .spawn(move || aggregator_loop.run())
                    .unwrap(),
            );
        }
    }

    pub fn request(&self, request: AggregatorRequest) -> bool {
        if request.roots_hashes.is_empty() {
            return false;
        }

        let request_len = request.roots_hashes.len();

        let added = {
            self.state
                .lock()
                .unwrap()
                .queue
                .push(request.channel.channel_id(), request)
        };

        if added {
            self.stats
                .inc(StatType::RequestAggregator, DetailType::Request);
            self.stats.add(
                StatType::RequestAggregator,
                DetailType::RequestHashes,
                request_len as u64,
            );
            self.condition.notify_one();
        } else {
            self.stats
                .inc(StatType::RequestAggregator, DetailType::Overfill);
            self.stats.add(
                StatType::RequestAggregator,
                DetailType::OverfillHashes,
                request_len as u64,
            );
        }

        // TODO: This stat is for compatibility with existing tests and is in principle unnecessary
        self.stats.inc(
            StatType::Aggregator,
            if added {
                DetailType::AggregatorAccepted
            } else {
                DetailType::AggregatorDropped
            },
        );

        added
    }

    pub fn stop(&self) {
        self.state.lock().unwrap().stopped = true;
        self.condition.notify_all();
        let mut threads = Vec::new();
        {
            let mut guard = self.threads.lock().unwrap();
            std::mem::swap(&mut threads, &mut *guard);
        }
        for thread in threads {
            thread.join().unwrap();
        }
    }

    /// Returns the number of currently queued request pools
    pub fn len(&self) -> usize {
        self.state.lock().unwrap().queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for RequestAggregator {
    fn drop(&mut self) {
        debug_assert!(self.threads.lock().unwrap().is_empty())
    }
}

impl ContainerInfoProvider for RequestAggregator {
    fn container_info(&self) -> ContainerInfo {
        let guard = self.state.lock().unwrap();
        ContainerInfo::builder()
            .node("queue", guard.queue.container_info())
            .finish()
    }
}

impl EventHandler<ChannelEvent> for RequestAggregator {
    fn handle(&self, event: &ChannelEvent) {
        if let ChannelEvent::Removed(id) = event {
            self.state.lock().unwrap().queue.remove(id);
        }
    }
}

#[derive(Clone)]
pub struct AggregatorRequest {
    pub channel: Arc<Channel>,
    pub roots_hashes: Vec<(BlockHash, Root)>,
    /// RAI: the epoch of the requester's elections
    pub epoch: ConsensusEpoch,
}

pub(crate) struct RequestAggregatorState {
    queue: FairQueue<ChannelId, AggregatorRequest>,
    stopped: bool,
}

struct RequestAggregatorLoop {
    mutex: Arc<Mutex<RequestAggregatorState>>,
    condition: Arc<Condvar>,
    stats: Arc<Stats>,
    config: RequestAggregatorConfig,
    ledger: Arc<Ledger>,
    vote_generators: Arc<VoteGenerators>,
    active_elections: Arc<AecService>,
    message_sender: Mutex<MessageSender>,
    evidence_replies: Arc<Mutex<EvidenceReplyCache>>,
}

impl RequestAggregatorLoop {
    fn run(&self) {
        let mut guard = self.mutex.lock().unwrap();
        while !guard.stopped {
            if !guard.queue.is_empty() {
                guard = self.run_batch(guard);
            } else {
                guard = self
                    .condition
                    .wait_while(guard, |g| !g.stopped && g.queue.is_empty())
                    .unwrap();
            }
        }
    }

    fn run_batch<'a>(
        &'a self,
        mut state: MutexGuard<'a, RequestAggregatorState>,
    ) -> MutexGuard<'a, RequestAggregatorState> {
        let batch = state.queue.next_batch(self.config.batch_size);
        drop(state);

        let mut any = self.ledger.any();

        for (_, request) in &batch {
            if any.should_refresh() {
                any = self.ledger.any();
            }

            let should_drop = request.channel.should_drop(TrafficType::VoteReply);

            if !should_drop {
                self.process(&any, request);
            } else {
                self.stats.inc_dir(
                    StatType::RequestAggregator,
                    DetailType::ChannelFull,
                    Direction::Out,
                );
            }
        }

        self.mutex.lock().unwrap()
    }

    fn process(&self, any: &dyn AnySet, request: &AggregatorRequest) {
        let mut served = HashSet::new();
        if cfg!(feature = "rai_protocol") {
            self.reply_with_fork_candidates(any, request);
            served = self.reply_with_certificates(request);
            self.join_requested_instances(any, request, &served);
        }

        let mut remaining = self.aggregate(any, request);
        // RAI: an epoch this node left gets its retained statements only
        // (served above); a fresh signature there would change the vote set
        // its frozen report committed to
        if cfg!(feature = "rai_protocol")
            && !self.active_elections.signs_account_votes_in(request.epoch)
        {
            self.stats.add_dir(
                StatType::Requests,
                DetailType::RequestsCannotVote,
                Direction::In,
                (remaining.remaining_normal.len() + remaining.remaining_final.len()) as u64,
            );
            return;
        }
        // RAI: a final vote is the exit statement of an instance, it is handed
        // out again only if this node made it, for the epoch it made it in. A
        // block cemented as a dependency has its instance still running here,
        // which answers with its own statements instead; a block without any
        // instance (genesis, a crawler's query) is answered as before.
        if cfg!(feature = "rai_protocol") {
            remaining.remaining_final.retain(|block| {
                let hash = block.hash();
                !served.contains(&hash)
                    && self
                        .active_elections
                        .final_voted_in_epoch(&hash, request.epoch)
                    || (!self.active_elections.is_finalized(&hash)
                        && !self
                            .active_elections
                            .is_active_root(&block.qualified_root()))
            });
        }

        if !remaining.remaining_normal.is_empty() {
            self.stats
                .inc(StatType::RequestAggregatorReplies, DetailType::NormalVote);

            // Generate votes for the remaining hashes
            let generated = self.vote_generators.generate_votes(
                &remaining.remaining_normal,
                &request.channel,
                request.epoch,
                VoteType::NonFinal,
            );
            self.stats.add_dir(
                StatType::Requests,
                DetailType::RequestsCannotVote,
                Direction::In,
                (remaining.remaining_normal.len() - generated) as u64,
            );
        }

        if !remaining.remaining_final.is_empty() {
            self.stats
                .inc(StatType::RequestAggregatorReplies, DetailType::FinalVote);

            // Generate final votes for the remaining hashes
            let generated = self.vote_generators.generate_votes(
                &remaining.remaining_final,
                &request.channel,
                request.epoch,
                VoteType::Final,
            );
            self.stats.add_dir(
                StatType::Requests,
                DetailType::RequestsCannotVote,
                Direction::In,
                (remaining.remaining_final.len() - generated) as u64,
            );
        }
    }

    /// RAI: a request names an instance (root, epoch) the requester runs. If
    /// this node has neither that instance nor finalized the block in that
    /// epoch, it joins the instance with its own ledger block for the root, so
    /// that every instance reaches the same outcome everywhere. The requester
    /// may hold a fork this node does not: a vote for that fork cannot open
    /// the instance here, the request can. Only blocks decided by a
    /// certificate here or still undecided have instances to join.
    fn join_requested_instances(
        &self,
        any: &dyn AnySet,
        request: &AggregatorRequest,
        served: &HashSet<BlockHash>,
    ) {
        if request.epoch > self.active_elections.current_epoch() {
            return;
        }
        for (hash, root) in &request.roots_hashes {
            if served.contains(hash) {
                continue;
            }
            let Some(block) = search_for_block(any, hash, root) else {
                continue;
            };
            let id = ElectionId::new(block.qualified_root(), request.epoch);
            if self.active_elections.election(&id).is_some()
                || self
                    .active_elections
                    .finalized_in_epoch(&block.hash(), request.epoch)
            {
                continue;
            }
            // A cemented block this node never decided by certificate (genesis,
            // a crawler's query) has no instance to join
            if any.confirmed().block_exists(&block.hash())
                && !self.active_elections.is_finalized(&block.hash())
            {
                continue;
            }
            self.active_elections.insert_for_vote(
                block,
                request.epoch,
                self.active_elections.now(),
            );
            self.stats.inc(
                StatType::RequestAggregatorReplies,
                DetailType::InstanceJoined,
            );
        }
    }

    /// Kudzu ships every block with the votes for it. Here a request for a
    /// block we do not hold, for a root where our ledger has a different
    /// successor, tells us that the requester lacks our fork candidate: it is
    /// published to the requester so that it can take its second look.
    fn reply_with_fork_candidates(&self, any: &dyn AnySet, request: &AggregatorRequest) {
        for (hash, root) in &request.roots_hashes {
            if any.block_exists(hash) {
                continue;
            }
            let Some(block) = search_for_block(any, hash, root) else {
                continue;
            };
            // RAI: the requester holds a candidate of this slot which this
            // node does not; it is asked for below
            let mut sender = self.message_sender.lock().unwrap();
            let mut replies = self.evidence_replies.lock().unwrap();
            if Self::publish_evidence_block(
                &mut sender,
                &mut replies,
                &request.channel,
                block.into(),
                Instant::now(),
            ) {
                self.stats.inc(
                    StatType::RequestAggregatorReplies,
                    DetailType::ForkCandidate,
                );
            }
        }
    }

    /// Kudzu: a block sent as evidence (a fork candidate, a certificate's
    /// block) goes on the reply queue the request was admitted against, not
    /// on the initial broadcast queue the publishing load fills up; and it
    /// counts as sent only once it is queued, so that the next request is
    /// answered if this one was dropped. Returns whether it was queued.
    fn publish_evidence_block(
        sender: &mut MessageSender,
        replies: &mut EvidenceReplyCache,
        channel: &Channel,
        block: Block,
        now: Instant,
    ) -> bool {
        let key = (channel.channel_id(), EvidenceId::Block(block.hash()));
        if !replies.should_send(key.clone(), now) {
            return false;
        }
        let publish = Message::Publish(Publish::new_evidence(block));
        let sent = sender.try_send(channel, &publish, TrafficType::VoteReply);
        if !sent {
            replies.forget(&key);
        }
        sent
    }

    /// Kudzu: a request for a block of a terminated election is answered with
    /// this node's own statements for that election, re-signed for exactly its
    /// candidates, plus the candidate blocks. Every representative is asked, so
    /// the requester assembles the certificates from small per-election votes
    /// instead of the original batches, which cover hundreds of other roots.
    /// Returns the requested hashes that were answered with evidence. The
    /// statements of all the instances asked about are batched into one vote
    /// per kind and representative: a statement's identity is
    /// (representative, kind, hash), the batch is the same set of statements
    /// and a fraction of the messages.
    fn reply_with_certificates(&self, request: &AggregatorRequest) -> HashSet<BlockHash> {
        let mut served = HashSet::new();
        let mut served_hashes = HashSet::new();
        let mut batches: HashMap<VoteKind, Vec<BlockHash>> = HashMap::new();
        let now = Instant::now();
        let channel_id = request.channel.channel_id();
        for (hash, _) in &request.roots_hashes {
            let Some((id, evidence)) = self
                .active_elections
                .certificate_evidence(hash, request.epoch)
            else {
                continue;
            };
            served_hashes.insert(*hash);
            if !served.insert(id) {
                continue;
            }
            let mut sender = self.message_sender.lock().unwrap();
            let mut replies = self.evidence_replies.lock().unwrap();
            // The candidates first, so that the votes find their election
            for block in evidence.blocks {
                Self::publish_evidence_block(
                    &mut sender,
                    &mut replies,
                    &request.channel,
                    block,
                    now,
                );
            }
            for (kind, hashes) in evidence.statements {
                if !replies.should_send(
                    (channel_id, EvidenceId::Statement(kind, hashes.clone())),
                    now,
                ) {
                    continue;
                }
                self.stats.inc(
                    StatType::RequestAggregatorReplies,
                    match kind {
                        VoteKind::First => DetailType::EvidenceFirst,
                        VoteKind::Notar => DetailType::EvidenceNotar,
                        VoteKind::Timeout => DetailType::EvidenceTimeout,
                        VoteKind::Abstain => DetailType::EvidenceAbstain,
                        VoteKind::Final => DetailType::EvidenceFinal,
                    },
                );
                let batch = batches.entry(kind).or_default();
                for hash in hashes {
                    if !batch.contains(&hash) {
                        batch.push(hash);
                    }
                }
            }
            self.stats.inc(
                StatType::RequestAggregatorReplies,
                DetailType::CertificateVotes,
            );
        }

        if !batches.is_empty() {
            let keys = self.vote_generators.rep_priv_keys();
            let mut sender = self.message_sender.lock().unwrap();
            for (kind, hashes) in batches {
                for chunk in hashes.chunks(Vote::MAX_HASHES) {
                    for key in &keys {
                        let vote = Vote::new_in_epoch(key, kind, request.epoch, chunk.to_vec());
                        let ack =
                            Message::ConfirmAck(ConfirmAck::new_with_certificate_evidence(vote));
                        sender.try_send(&request.channel, &ack, TrafficType::Vote);
                    }
                }
            }
        }
        served_hashes
    }

    /// Aggregate requests and send cached votes to channel.
    /// Return the remaining hashes that need vote generation for each block for regular & final vote generators
    fn aggregate(&self, any: &dyn AnySet, requests: &AggregatorRequest) -> AggregateResult {
        let mut aggregator = RequestAggregatorImpl::new(&self.stats, any);
        aggregator.add_votes(&requests.roots_hashes);
        aggregator.get_result()
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum EvidenceId {
    Block(BlockHash),
    Statement(VoteKind, Vec<BlockHash>),
}

/// Kudzu: bounds how often the same certificate evidence is sent to a peer.
/// A retained vote batch may support hundreds of roots, so it must not be
/// resent for every one of them.
#[derive(Default)]
struct EvidenceReplyCache {
    sent: HashMap<(ChannelId, EvidenceId), Instant>,
    last_prune: Option<Instant>,
}

impl EvidenceReplyCache {
    /// Shorter than the solicitation round (5 s), so that every round of a
    /// replica that still lacks the statement is answered
    const INTERVAL: Duration = Duration::from_secs(2);
    const MAX_ENTRIES: usize = 65536;

    fn should_send(&mut self, key: (ChannelId, EvidenceId), now: Instant) -> bool {
        if self
            .last_prune
            .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1))
        {
            self.sent
                .retain(|_, t| now.duration_since(*t) < Self::INTERVAL);
            self.last_prune = Some(now);
        }
        let due = self
            .sent
            .get(&key)
            .is_none_or(|t| now.duration_since(*t) >= Self::INTERVAL);
        if due && self.sent.len() < Self::MAX_ENTRIES {
            self.sent.insert(key, now);
        }
        due
    }

    /// The evidence was not sent after all: the next request gets it
    fn forget(&mut self, key: &(ChannelId, EvidenceId)) {
        self.sent.remove(key);
    }
}

#[cfg(test)]
mod evidence_reply_cache_tests {
    use super::*;

    #[test]
    fn resends_only_after_the_interval() {
        let mut cache = EvidenceReplyCache::default();
        let key = (ChannelId::from(1), EvidenceId::Block(BlockHash::from(1)));
        let now = Instant::now();
        assert!(cache.should_send(key.clone(), now));
        assert!(!cache.should_send(key.clone(), now + Duration::from_secs(1)));
        assert!(cache.should_send(key.clone(), now + EvidenceReplyCache::INTERVAL));
        // Other peers and other evidence are independent
        assert!(cache.should_send((ChannelId::from(2), key.1.clone()), now));
        assert!(cache.should_send((key.0, EvidenceId::Block(BlockHash::from(2))), now));
    }

    #[test]
    fn forgotten_evidence_is_due_again() {
        let mut cache = EvidenceReplyCache::default();
        let key = (ChannelId::from(1), EvidenceId::Block(BlockHash::from(1)));
        let now = Instant::now();
        assert!(cache.should_send(key.clone(), now));
        cache.forget(&key);
        assert!(cache.should_send(key.clone(), now));
    }
}
