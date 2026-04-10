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
            channel_waiter::ChannelWaiterStats,
            priority::{PullCountDecider, PullTypeDecider, QueryFactory},
            query_sender::QuerySender,
        },
        state::BootstrapLogic,
    },
    transport::MessageSender,
};
use rand::RngCore;
use rsnano_ledger::{BlockSource, Ledger};
use rsnano_nullable_random::NullableRngFactory;

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
                config,
                clock: SteadyClock::default(),
                stats2,
                network,
                limiter,
                block_processor_queue,
                query_factory,
                rng_factory: NullableRngFactory::default(),
            },
        }
    }

    pub fn run_loop(&mut self) {
        let mut state = self.state.lock().unwrap();
        let mut loop_counter = 0;
        while !state.stopped {
            let mut produced = 0;

            //            if self.config.enable_frontier_scan
            //                && let Some(spec) = self.try_frontier_query(&mut state, now) {
            //                self.query_sender.send(spec, &mut state);
            //                produced += 1;
            //            }
            if self.config.enable_priorities
                && let Some(spec) = self.query_factory2.try_priority_query(&mut state)
            {
                self.query_sender.send(spec, &mut state);
                produced += 1;
            }
            //            if self.config.enable_dependency_walker
            //                && let Some(spec) = self.try_dependency_query(&mut state, now) {
            //                self.query_sender.send(spec, &mut state);
            //                produced += 1;
            //            }

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
    stats2: Arc<PriorityRequesterStats>,
    network: Arc<RwLock<Network>>,
    limiter: Arc<Mutex<TokenBucket>>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    query_factory: QueryFactory,
    rng_factory: NullableRngFactory,
}

impl QueryFactory2 {
    fn try_priority_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
        self.stats2.loop_count.fetch_add(1, Ordering::Relaxed);
        let now = self.clock.now();
        if !self.block_processor_free() {
            self.stats2
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
            self.stats2.wait_priority.fetch_add(1, Ordering::Relaxed);
        } else {
            self.stats2.next.fetch_add(1, Ordering::Relaxed);
        }
        query
    }

    fn acquire_channel(&self, state: &mut BootstrapLogic, now: Timestamp) -> Option<Arc<Channel>> {
        if state.running_queries.len() >= self.config.max_requests {
            return None;
        }
        if !self.limiter.lock().unwrap().try_consume(1, now) {
            return None;
        }
        let network = self.network.read().unwrap();
        let candidates: Vec<_> = network
            .available_channels(TrafficType::BootstrapRequests)
            .map(|c| c.channel_id())
            .collect();
        // TODO refactor so that the running queries isn't incremented here
        let id = state.scoring.channel(candidates)?;
        network.get(id).cloned()
    }

    fn block_processor_free(&self) -> bool {
        self.block_processor_queue.queue_len(BlockSource::Bootstrap)
            < self.config.block_processor_threshold
    }
}

#[derive(Default)]
pub(crate) struct PriorityRequesterStats {
    pub loop_count: AtomicU64,
    pub wait_block_processor: AtomicU64,
    pub wait_priority: AtomicU64,
    pub channel_waiter: Arc<ChannelWaiterStats>,
    pub next: AtomicU64,
}

impl StatsSource for PriorityRequesterStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const STAT_NAME: &str = "boot_requester_prio";

        result.insert(STAT_NAME, "loop", self.loop_count.load(Ordering::Relaxed));
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
            "bootstrap_next",
            "next_priority",
            self.next.load(Ordering::Relaxed),
        );

        self.channel_waiter.collect_stats(STAT_NAME, result);
    }
}
