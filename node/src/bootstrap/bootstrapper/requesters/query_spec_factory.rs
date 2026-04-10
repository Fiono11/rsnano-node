use std::sync::{Arc, RwLock, atomic::Ordering::Relaxed};

use rand::RngCore;

use rsnano_ledger::{BlockSource, Ledger};
use rsnano_messages::{AscPullReqType, FrontiersReqPayload};
use rsnano_network::{Channel, Network, TrafficType, token_bucket::TokenBucket};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_random::NullableRngFactory;
use rsnano_types::{Account, BlockHash};

use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::bootstrapper::{
        AscPullQuerySpec, BootstrapConfig, PromiseContext,
        requesters::{
            priority::{PullCountDecider, PullTypeDecider, QueryFactory},
            stats::BootstrapRequesterStats,
        },
        state::BootstrapLogic,
    },
};

pub(crate) struct QuerySpecFactory {
    config: BootstrapConfig,
    clock: SteadyClock,
    stats: Arc<BootstrapRequesterStats>,
    network: Arc<RwLock<Network>>,
    request_limiter: TokenBucket,
    block_processor_queue: Arc<BlockProcessorQueue>,
    query_factory: QueryFactory,
    rng_factory: NullableRngFactory,
    frontiers_limiter: TokenBucket,
}

impl QuerySpecFactory {
    pub fn new(
        config: BootstrapConfig,
        stats: Arc<BootstrapRequesterStats>,
        network: Arc<RwLock<Network>>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
    ) -> Self {
        let pull_type_decider = PullTypeDecider::new(config.optimistic_request_percentage);
        let pull_count_decider = PullCountDecider::new(config.max_pull_count);
        let query_factory = QueryFactory::new(ledger, pull_type_decider, pull_count_decider);
        let limiter = TokenBucket::new(config.rate_limit);
        Self {
            clock: SteadyClock::default(),
            stats,
            network,
            request_limiter: limiter,
            block_processor_queue,
            query_factory,
            rng_factory: NullableRngFactory::default(),
            frontiers_limiter: TokenBucket::new(config.frontier_rate_limit),
            config,
        }
    }

    pub fn try_priority_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
        let now = self.clock.now();
        if !self.block_processor_free() {
            self.stats.wait_block_processor.fetch_add(1, Relaxed);
            return None;
        }
        let channel = self.acquire_channel(state, now)?;
        let id = self.rng_factory.rng().next_u64();
        let mut context = PromiseContext {
            logic: state,
            now,
            id,
        };
        let query = self
            .query_factory
            .next_priority_query(&mut context, channel);
        if query.is_none() {
            self.stats.wait_priority.fetch_add(1, Relaxed);
        } else {
            self.stats.next.fetch_add(1, Relaxed);
        }
        query
    }

    pub fn try_dependency_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
        let now = self.clock.now();
        let channel = self.acquire_channel(state, now)?;
        let id = self.rng_factory.rng().next_u64();
        match state.next_blocked_query(id, &channel) {
            Some(spec) => {
                // TODO stats
                Some(spec)
            }
            None => {
                // TODO stats
                None
            }
        }
    }

    pub fn try_frontier_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
        let now = self.clock.now();
        if state.bootstrap_queue.queue_half_full() {
            return None;
        }
        if !self.frontiers_limiter.could_consume(1, now) {
            return None;
        }

        if state.frontiers_processor.frontier_checker_overfill() {
            return None;
        }
        let channel = self.acquire_channel(state, now)?;

        let start = state.frontiers_processor.next(now);
        if !start.is_zero() {
            // TODO stats
            self.frontiers_limiter.consume(1, now);
            let id = self.rng_factory.rng().next_u64();
            Some(Self::create_frontier_query_spec(&channel, start, id))
        } else {
            // TODO stats
            None
        }
    }

    fn acquire_channel(
        &mut self,
        state: &mut BootstrapLogic,
        now: Timestamp,
    ) -> Option<Arc<Channel>> {
        if state.running_queries.len() >= self.config.max_requests {
            self.stats.queries_overfill.fetch_add(1, Relaxed);
            return None;
        }

        if !self.request_limiter.could_consume(1, now) {
            self.stats.rate_limit.fetch_add(1, Relaxed);
            return None;
        }

        let network = self.network.read().unwrap();
        let candidates: Vec<_> = network
            .available_channels(TrafficType::BootstrapRequests)
            .map(|c| c.channel_id())
            .collect();
        // TODO refactor so that the running queries isn't incremented here
        let Some(id) = state.scoring.channel(candidates) else {
            self.stats.no_candidate.fetch_add(1, Relaxed);
            return None;
        };
        let channel = network.get(id).cloned();
        if channel.is_none() {
            self.stats.no_candidate.fetch_add(1, Relaxed);
        } else {
            self.request_limiter.consume(1, now);
        }
        channel
    }

    fn block_processor_free(&self) -> bool {
        self.block_processor_queue.queue_len(BlockSource::Bootstrap)
            < self.config.block_processor_threshold
    }

    fn create_frontier_query_spec(
        channel: &Arc<Channel>,
        start: Account,
        query_id: u64,
    ) -> AscPullQuerySpec {
        let request = Self::request_frontiers(start);
        AscPullQuerySpec {
            query_id,
            channel: channel.clone(),
            req_type: request,
            account: Account::ZERO,
            hash: BlockHash::ZERO,
            cooldown_account: false,
        }
    }

    fn request_frontiers(start: Account) -> AscPullReqType {
        AscPullReqType::Frontiers(FrontiersReqPayload {
            start,
            count: FrontiersReqPayload::MAX_FRONTIERS,
        })
    }
}
