use std::{
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use rsnano_network::{Channel, Network, TrafficType, token_bucket::TokenBucket};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_utils::stats::{Stats, StatsCollection, StatsSource};

use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::bootstrapper::{
        AscPullQuerySpec, BootstrapConfig, PromiseContext,
        requesters::{
            priority::{PullCountDecider, PullTypeDecider, QueryFactory},
            query_sender::QuerySender,
        },
        state::BootstrapLogic,
    },
    transport::MessageSender,
};
use rand::RngCore;
use rsnano_ledger::{BlockSource, Ledger};
use rsnano_messages::{AscPullReqType, FrontiersReqPayload};
use rsnano_nullable_random::NullableRngFactory;
use rsnano_types::{Account, BlockHash};

pub(super) struct RequesterLoop {
    state: Arc<Mutex<BootstrapLogic>>,
    state_changed: Arc<Condvar>,
    config: BootstrapConfig,
    query_sender: QuerySender,
    query_factory2: QueryFactory2,
}

impl RequesterLoop {
    const THROTTLE_WAIT: Duration = Duration::from_millis(50);

    pub(super) fn new(
        logic: Arc<Mutex<BootstrapLogic>>,
        state_changed: Arc<Condvar>,
        config: BootstrapConfig,
        message_sender: MessageSender,
        stats: Arc<Stats>,
        stats2: Arc<PriorityRequesterStats>,
        network: Arc<RwLock<Network>>,
        limiter: Arc<Mutex<TokenBucket>>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
    ) -> Self {
        let pull_type_decider = PullTypeDecider::new(config.optimistic_request_percentage);
        let pull_count_decider = PullCountDecider::new(config.max_pull_count);
        let query_factory = QueryFactory::new(ledger, pull_type_decider, pull_count_decider);
        Self {
            state: logic,
            state_changed,
            config: config.clone(),
            query_sender: QuerySender::new(message_sender, stats.clone()),
            query_factory2: QueryFactory2 {
                clock: SteadyClock::default(),
                stats: stats2,
                network,
                limiter,
                block_processor_queue,
                query_factory,
                rng_factory: NullableRngFactory::default(),
                frontiers_limiter: TokenBucket::new(config.frontier_rate_limit),
                config,
            },
        }
    }

    pub fn run_loop(&mut self) {
        let mut state = self.state.lock().unwrap();
        let mut loop_counter = 0;
        while !state.stopped {
            let mut produced = 0;

            if self.config.enable_priorities
                && let Some(spec) = self.query_factory2.try_priority_query(&mut state)
            {
                self.query_sender.send(spec, &mut state);
                produced += 1;
            }
            if self.config.enable_dependency_walker
                && let Some(spec) = self.query_factory2.try_dependency_query(&mut state)
            {
                self.query_sender.send(spec, &mut state);
                produced += 1;
            }
            if self.config.enable_frontier_scan
                && let Some(spec) = self.query_factory2.try_frontier_query(&mut state)
            {
                self.query_sender.send(spec, &mut state);
                produced += 1;
            }

            if produced == 0 {
                // nothing to do — wait for a state change or fixed throttle
                state = self
                    .state_changed
                    .wait_timeout_while(state, Self::THROTTLE_WAIT, |s| !s.stopped)
                    .unwrap()
                    .0;
            } else {
                loop_counter += 1;
                if loop_counter > 16 {
                    loop_counter = 0;
                    // periodically release the lock so cleanup/response threads can run
                    drop(state);
                    std::thread::yield_now();
                    state = self.state.lock().unwrap();
                }
            }
        }
    }
}

struct QueryFactory2 {
    config: BootstrapConfig,
    clock: SteadyClock,
    stats: Arc<PriorityRequesterStats>,
    network: Arc<RwLock<Network>>,
    limiter: Arc<Mutex<TokenBucket>>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    query_factory: QueryFactory,
    rng_factory: NullableRngFactory,
    frontiers_limiter: TokenBucket,
}

impl QueryFactory2 {
    fn try_priority_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
        let now = self.clock.now();
        if !self.block_processor_free() {
            self.stats
                .wait_block_processor
                .fetch_add(1, Ordering::Relaxed);
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
            self.stats.wait_priority.fetch_add(1, Ordering::Relaxed);
        } else {
            self.stats.next.fetch_add(1, Ordering::Relaxed);
        }
        query
    }

    fn try_dependency_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
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

    fn try_frontier_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
        let now = self.clock.now();
        if state.bootstrap_queue.queue_half_full() {
            return None;
        }
        if !self.frontiers_limiter.try_consume(1, now) {
            return None;
        }
        if state.frontiers_processor.frontier_checker_overfill() {
            return None;
        }
        let channel = self.acquire_channel(state, now)?;

        let start = state.frontiers_processor.next(now);
        if !start.is_zero() {
            // TODO stats
            let id = self.rng_factory.rng().next_u64();
            Some(Self::create_query_spec(&channel, start, id))
        } else {
            // TODO stats
            None
        }
    }

    fn acquire_channel(&self, state: &mut BootstrapLogic, now: Timestamp) -> Option<Arc<Channel>> {
        if state.running_queries.len() >= self.config.max_requests {
            self.stats.queries_overfill.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        // TODO refactor so that we don't change the rate limiter here
        if !self.limiter.lock().unwrap().try_consume(1, now) {
            self.stats.rate_limit.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let network = self.network.read().unwrap();
        let candidates: Vec<_> = network
            .available_channels(TrafficType::BootstrapRequests)
            .map(|c| c.channel_id())
            .collect();
        // TODO refactor so that the running queries isn't incremented here
        let Some(id) = state.scoring.channel(candidates) else {
            self.stats.no_candidate.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let channel = network.get(id).cloned();
        if channel.is_none() {
            self.stats.no_candidate.fetch_add(1, Ordering::Relaxed);
        }
        channel
    }

    fn block_processor_free(&self) -> bool {
        self.block_processor_queue.queue_len(BlockSource::Bootstrap)
            < self.config.block_processor_threshold
    }

    fn create_query_spec(
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

#[derive(Default)]
pub(crate) struct PriorityRequesterStats {
    pub wait_block_processor: AtomicU64,
    pub wait_priority: AtomicU64,
    pub next: AtomicU64,
    pub no_candidate: AtomicU64,
    pub queries_overfill: AtomicU64,
    pub rate_limit: AtomicU64,
}

impl StatsSource for PriorityRequesterStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const STAT_NAME: &str = "boot_requester";

        result.insert(
            STAT_NAME,
            "wait_block_processor",
            self.wait_block_processor.load(Ordering::Relaxed),
        );
        result.insert(
            STAT_NAME,
            "wait_priority",
            self.wait_priority.load(Ordering::Relaxed),
        );

        result.insert(
            STAT_NAME,
            "next_priority",
            self.next.load(Ordering::Relaxed),
        );

        result.insert(
            STAT_NAME,
            "no_candidate",
            self.no_candidate.load(Ordering::Relaxed),
        );

        result.insert(
            STAT_NAME,
            "queries_overfill",
            self.queries_overfill.load(Ordering::Relaxed),
        );

        result.insert(
            STAT_NAME,
            "rate_limit",
            self.queries_overfill.load(Ordering::Relaxed),
        );
    }
}
