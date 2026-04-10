mod bootstrap_promise_runner;
mod channel_waiter;
mod dependency_requester;
mod frontier_requester;
mod priority;
mod query_sender;
mod requester_loop;
mod send_queries_promise;

use std::{
    sync::{Arc, Condvar, Mutex, RwLock},
    thread::JoinHandle,
};

use rsnano_ledger::Ledger;
use rsnano_network::Network;
use rsnano_network::token_bucket::TokenBucket;
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_random::NullableRngFactory;
use rsnano_utils::stats::{Stats, StatsCollection, StatsSource};

use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::bootstrapper::{
        AscPullQuerySpec, BootstrapConfig, BootstrapPromise,
        requesters::requester_loop::{PriorityRequesterStats, RequesterLoop},
        state::BootstrapLogic,
    },
    transport::MessageSender,
};

use {
    bootstrap_promise_runner::BootstrapPromiseRunner, channel_waiter::ChannelWaiter,
    dependency_requester::DependencyRequester, frontier_requester::FrontierRequester,
    query_sender::QuerySender, send_queries_promise::SendQueriesPromise,
};

/// Manages the threads that send out AscPullReqs
pub(crate) struct Requesters {
    limiter: Arc<Mutex<TokenBucket>>,
    config: BootstrapConfig,
    stats: Arc<Stats>,
    message_sender: MessageSender,
    state: Arc<Mutex<BootstrapLogic>>,
    state_changed: Arc<Condvar>,
    clock: Arc<SteadyClock>,
    threads: Mutex<Option<RequesterThreads>>,
    ledger: Arc<Ledger>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    network: Arc<RwLock<Network>>,
    stats_sources: Mutex<Vec<Arc<dyn StatsSource + Send + Sync>>>,
}

impl Requesters {
    pub(crate) fn new(
        config: BootstrapConfig,
        stats: Arc<Stats>,
        message_sender: MessageSender,
        state: Arc<Mutex<BootstrapLogic>>,
        state_changed: Arc<Condvar>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
        network: Arc<RwLock<Network>>,
    ) -> Self {
        Self {
            limiter: Arc::new(Mutex::new(TokenBucket::new(config.rate_limit))),
            config,
            stats,
            message_sender,
            state,
            state_changed,
            clock: Arc::new(SteadyClock::new_null()),
            ledger,
            block_processor_queue,
            network,
            threads: Mutex::new(None),
            stats_sources: Mutex::new(Vec::new()),
        }
    }

    pub fn start(&self) {
        let limiter = self.limiter.clone();
        let max_requests = self.config.max_requests;
        let channel_waiter =
            ChannelWaiter::new(self.network.clone(), limiter.clone(), max_requests);

        let runner = Arc::new(BootstrapPromiseRunner {
            state: self.state.clone(),
            throttle_wait: self.config.throttle_wait,
            state_changed: self.state_changed.clone(),
            clock: self.clock.clone(),
            rng_factory: NullableRngFactory::default(),
        });

        let frontiers = if self.config.enable_frontier_scan {
            Some(self.spawn_query(
                "Bootstrap front",
                FrontierRequester::new(
                    self.stats.clone(),
                    self.clock.clone(),
                    self.config.frontier_rate_limit,
                    channel_waiter.clone(),
                ),
                runner.clone(),
            ))
        } else {
            None
        };

        let join_handle = if self.config.enable_priorities {
            let requester_stats = Arc::new(PriorityRequesterStats::default());
            self.stats_sources
                .lock()
                .unwrap()
                .push(requester_stats.clone());

            let mut requester_loop = RequesterLoop::new(
                self.state.clone(),
                self.state_changed.clone(),
                self.config.clone(),
                self.message_sender.clone(),
                self.stats.clone(),
                requester_stats,
                self.network.clone(),
                self.limiter.clone(),
                self.ledger.clone(),
                self.block_processor_queue.clone(),
            );
            Some(
                std::thread::Builder::new()
                    .name("Bootstrap".to_string())
                    .spawn(move || {
                        requester_loop.run_loop();
                    })
                    .unwrap(),
            )
        } else {
            None
        };

        let dependencies = if self.config.enable_dependency_walker {
            //let requester = DependencyRequester::new(channel_waiter);
            //self.stats_sources.lock().unwrap().push(requester.stats());
            //Some(self.spawn_query("Bootstrap walkr", requester, runner.clone()))
            None
        } else {
            None
        };

        let requesters = RequesterThreads {
            frontiers,
            dependencies,
            join_handle,
        };

        *self.threads.lock().unwrap() = Some(requesters);
    }

    pub fn stop(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.stopped = true;
        }
        self.state_changed.notify_all();

        let threads = self.threads.lock().unwrap().take();
        if let Some(mut threads) = threads {
            threads.join();
        }
    }

    fn spawn_query<T>(
        &self,
        name: impl Into<String>,
        query_factory: T,
        runner: Arc<BootstrapPromiseRunner>,
    ) -> JoinHandle<()>
    where
        T: BootstrapPromise<AscPullQuerySpec> + Send + 'static,
    {
        let mut query_sender = QuerySender::new(self.message_sender.clone(), self.stats.clone());
        query_sender.set_request_timeout(self.config.request_timeout);

        let send_promise = SendQueriesPromise::new(query_factory, query_sender);

        std::thread::Builder::new()
            .name(name.into())
            .spawn(Box::new(move || {
                runner.run(send_promise);
            }))
            .unwrap()
    }
}

pub struct RequesterThreads {
    pub join_handle: Option<JoinHandle<()>>,
    pub dependencies: Option<JoinHandle<()>>,
    pub frontiers: Option<JoinHandle<()>>,
}

impl RequesterThreads {
    pub fn join(&mut self) {
        if let Some(handle) = self.join_handle.take() {
            handle.join().unwrap();
        }
        if let Some(dependencies) = self.dependencies.take() {
            dependencies.join().unwrap();
        }
        if let Some(frontiers) = self.frontiers.take() {
            frontiers.join().unwrap();
        }
    }
}

impl StatsSource for Requesters {
    fn collect_stats(&self, result: &mut StatsCollection) {
        for s in self.stats_sources.lock().unwrap().iter() {
            s.collect_stats(result);
        }
    }
}
