use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::JoinHandle,
};

#[cfg(feature = "rai_protocol")]
use std::collections::HashSet;

use rsnano_ledger::{AnySet, Ledger};
use rsnano_network::{Channel, ChannelEvent, ChannelId, TrafficType};
use rsnano_types::{BlockHash, Root};
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    fair_queue::FairQueue,
    stats::{DetailType, Direction, StatType, Stats},
};

use super::{
    VoteGenerators,
    request_aggregator_impl::{AggregateResult, RequestAggregatorImpl},
};
use crate::consensus::election::VoteType;

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
    state: Arc<Mutex<RequestAggregatorState>>,
    condition: Arc<Condvar>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    #[cfg(feature = "rai_protocol")]
    active_elections: Arc<crate::consensus::AecService>,
}

impl RequestAggregator {
    pub fn new(
        config: RequestAggregatorConfig,
        stats: Arc<Stats>,
        vote_generators: Arc<VoteGenerators>,
        ledger: Arc<Ledger>,
        #[cfg(feature = "rai_protocol")] active_elections: Arc<crate::consensus::AecService>,
    ) -> Self {
        let max_queue = config.max_queue;
        Self {
            stats,
            vote_generators,
            ledger,
            config,
            condition: Arc::new(Condvar::new()),
            state: Arc::new(Mutex::new(RequestAggregatorState {
                queue: FairQueue::new(move |_| max_queue, |_| 1),
                stopped: false,
            })),
            #[cfg(feature = "rai_protocol")]
            active_elections,
            threads: Mutex::new(Vec::new()),
        }
    }

    pub fn new_null() -> Self {
        Self::new(
            RequestAggregatorConfig::new(1),
            Stats::default().into(),
            VoteGenerators::new_null().into(),
            Ledger::new_null().into(),
            #[cfg(feature = "rai_protocol")]
            crate::consensus::AecService::new_null().into(),
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
                #[cfg(feature = "rai_protocol")]
                active_elections: self.active_elections.clone(),
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

    /// Reply to a repair request from durable RAI finalization state. This
    /// deliberately bypasses ordinary aggregation, whose final-vote decision
    /// does not retain the original RAI election identity.
    #[cfg(feature = "rai_protocol")]
    pub(crate) fn generate_rai_final_vote(
        &self,
        hash: &BlockHash,
        root: &Root,
        epoch: rsnano_types::RaiEpoch,
        channel: &Arc<Channel>,
    ) -> usize {
        let Some(target) =
            self.active_elections
                .rai_finalized_vote_target(&self.ledger, hash, root, epoch)
        else {
            return 0;
        };
        self.vote_generators
            .generate_rai_final_vote(&target, channel)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn generate_rai_notar_vote(
        &self,
        _hash: &BlockHash,
        root: &Root,
        epoch: rsnano_types::RaiEpoch,
        channel: &Arc<Channel>,
    ) -> usize {
        let Some((terminal_hash, metadata)) = self
            .active_elections
            .rai_terminal_notarized_target_for_root(root, epoch)
        else {
            return 0;
        };
        let target = rsnano_ledger::RaiFinalizedVoteTarget {
            election_id: metadata.election_id.clone(),
            hash: terminal_hash,
            root: *root,
            metadata,
        };
        self.vote_generators
            .generate_rai_notar_vote(&target, channel)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn generate_rai_active_slot_vote(
        &self,
        root: &Root,
        epoch: rsnano_types::RaiEpoch,
        channel: &Arc<Channel>,
    ) -> usize {
        let Some(target) = self
            .active_elections
            .rai_active_slot_vote_target_for_root(root, epoch)
        else {
            return 0;
        };
        self.vote_generators
            .generate_rai_notar_vote(&target, channel)
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
    #[cfg(feature = "rai_protocol")]
    active_elections: Arc<crate::consensus::AecService>,
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
        #[cfg(feature = "rai_protocol")]
        let mut rai_slot_contexts = Vec::new();
        #[cfg(feature = "rai_protocol")]
        let mut rai_slot_targets = Vec::new();
        #[cfg(feature = "rai_protocol")]
        let mut seen_rai_slots = HashSet::new();
        #[cfg(feature = "rai_protocol")]
        let ordinary = request
            .roots_hashes
            .iter()
            .filter_map(|(hash, root)| {
                if hash.is_zero()
                    && let Some(metadata) =
                        self.active_elections.rai_close_vote_context_for_root(root)
                {
                    self.vote_generators.reply_cached_rai_election_votes(
                        root,
                        &metadata,
                        &request.channel,
                    );
                    if let Some(target) = self
                        .active_elections
                        .rai_active_close_vote_target_for_root(root)
                    {
                        if target.metadata.phase == rsnano_types::RaiVotePhase::Final {
                            self.vote_generators
                                .generate_rai_final_vote(&target, &request.channel);
                        } else {
                            self.vote_generators
                                .generate_rai_notar_vote(&target, &request.channel);
                        }
                    }
                    return None;
                }
                if hash.is_zero()
                    && let Some(metadata) =
                        self.active_elections.rai_slot_vote_context_for_root(root)
                {
                    if seen_rai_slots.insert((*root, metadata.election_id.clone())) {
                        if let Some(target) = self
                            .active_elections
                            .rai_active_slot_vote_target_for_root(root, metadata.epoch)
                        {
                            rai_slot_targets.push(target);
                        }
                        rai_slot_contexts.push((*root, metadata));
                    }
                    return None;
                }
                // Ordinary nonzero requests flow through aggregate/generate.
                // The generator replays cached signed batches once per request
                // and then generates any missing current-phase leaves. Eagerly
                // replying here once per hash would retransmit the same full
                // vectorized batch for every leaf it contains.
                Some((*hash, *root))
            })
            .collect::<Vec<_>>();
        #[cfg(feature = "rai_protocol")]
        self.vote_generators
            .reply_cached_and_generate_rai_slot_votes(
                &rai_slot_contexts,
                &rai_slot_targets,
                &request.channel,
            );
        #[cfg(feature = "rai_protocol")]
        let ordinary_request = AggregatorRequest {
            channel: request.channel.clone(),
            roots_hashes: ordinary,
        };
        #[cfg(feature = "rai_protocol")]
        let request = &ordinary_request;

        let remaining = self.aggregate(any, request);

        if !remaining.remaining_normal.is_empty() {
            self.stats
                .inc(StatType::RequestAggregatorReplies, DetailType::NormalVote);

            // Generate votes for the remaining hashes
            let generated = self.vote_generators.generate_votes(
                &remaining.remaining_normal,
                &request.channel,
                VoteType::NonFinal,
                #[cfg(feature = "rai_protocol")]
                &self.rai_contexts(&remaining.remaining_normal),
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

            // Generate final votes only when the original RAI election context
            // is known. Contextless requests for confirmed blocks are
            // representative-crawler challenges and receive a discovery-only
            // signature which cannot enter consensus state.
            #[cfg(feature = "rai_protocol")]
            let generated = {
                let contexts = self.optional_rai_contexts(&remaining.remaining_final);
                let mut contextual = Vec::with_capacity(remaining.remaining_final.len());
                let mut contextual_contexts = Vec::with_capacity(contexts.len());
                let mut discovery = Vec::new();
                for (block, context) in remaining.remaining_final.iter().cloned().zip(contexts) {
                    if let Some(context) = context {
                        contextual.push(block);
                        contextual_contexts.push(context);
                    } else {
                        discovery.push(block);
                    }
                }

                self.vote_generators.generate_votes(
                    &contextual,
                    &request.channel,
                    VoteType::Final,
                    &contextual_contexts,
                ) + self
                    .vote_generators
                    .generate_rai_discovery_votes(&discovery, &request.channel)
            };

            #[cfg(not(feature = "rai_protocol"))]
            let generated = self.vote_generators.generate_votes(
                &remaining.remaining_final,
                &request.channel,
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

    #[cfg(feature = "rai_protocol")]
    fn rai_contexts(
        &self,
        blocks: &[rsnano_types::SavedBlock],
    ) -> Vec<(rsnano_types::RaiVoteMetadata, bool)> {
        self.optional_rai_contexts(blocks)
            .into_iter()
            .map(Option::unwrap_or_default)
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    fn optional_rai_contexts(
        &self,
        blocks: &[rsnano_types::SavedBlock],
    ) -> Vec<Option<(rsnano_types::RaiVoteMetadata, bool)>> {
        let hashes = blocks.iter().map(|block| block.hash()).collect::<Vec<_>>();
        self.active_elections.rai_vote_contexts(&hashes)
    }

    /// Aggregate requests and send cached votes to channel.
    /// Return the remaining hashes that need vote generation for each block for regular & final vote generators
    fn aggregate(&self, any: &dyn AnySet, requests: &AggregatorRequest) -> AggregateResult {
        let mut aggregator = RequestAggregatorImpl::new(&self.stats, any);
        aggregator.add_votes(&requests.roots_hashes);
        aggregator.get_result()
    }
}
