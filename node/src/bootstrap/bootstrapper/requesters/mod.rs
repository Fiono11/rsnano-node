mod pull_count_decider;
mod pull_type_decider;
mod query_factory;
mod query_sender;
mod requester_loop;
mod stats;

pub(crate) use pull_count_decider::PullCountDecider;
pub(crate) use pull_type_decider::{PullType, PullTypeDecider};

use std::{
    sync::{Arc, Mutex, RwLock},
    thread::JoinHandle,
};

use rsnano_ledger::Ledger;
use rsnano_network::Network;
use rsnano_utils::stats::{Stats, StatsCollection, StatsSource};

use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::bootstrapper::{
        BootstrapConfig, StoppedFlag,
        bootstrap_queue::BootstrapQueue,
        frontier_scan::frontier_check_pool::FrontierCheckPool,
        query_tracker::QueryTracker,
        requesters::{requester_loop::RequesterLoop, stats::BootstrapRequesterStats},
    },
    transport::MessageSender,
};
use rsnano_nullable_condvar::NullableCondvarMutex;

/// Manages the threads that send out AscPullReqs
pub(crate) struct Requesters {
    config: BootstrapConfig,
    stats: Arc<Stats>,
    message_sender: MessageSender,
    query_tracker: Arc<QueryTracker>,
    thread: Mutex<Option<JoinHandle<()>>>,
    ledger: Arc<Ledger>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    bootstrap_queue: Arc<BootstrapQueue>,
    network: Arc<RwLock<Network>>,
    stats_sources: Mutex<Vec<Arc<dyn StatsSource + Send + Sync>>>,
    frontier_check_pool: Arc<FrontierCheckPool>,
    stopped: Arc<NullableCondvarMutex<StoppedFlag>>,
}

impl Requesters {
    pub(crate) fn new(
        config: BootstrapConfig,
        stats: Arc<Stats>,
        message_sender: MessageSender,
        query_tracker: Arc<QueryTracker>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
        bootstrap_queue: Arc<BootstrapQueue>,
        network: Arc<RwLock<Network>>,
        frontier_check_pool: Arc<FrontierCheckPool>,
    ) -> Self {
        Self {
            config,
            stats,
            message_sender,
            query_tracker,
            ledger,
            block_processor_queue,
            bootstrap_queue,
            network,
            thread: Mutex::new(None),
            stats_sources: Mutex::new(Vec::new()),
            frontier_check_pool,
            stopped: Arc::new(NullableCondvarMutex::new(StoppedFlag::default())),
        }
    }

    pub fn start(&self) {
        let requester_stats = Arc::new(BootstrapRequesterStats::default());
        self.stats_sources
            .lock()
            .unwrap()
            .push(requester_stats.clone());

        let mut requester_loop = RequesterLoop::new(
            self.query_tracker.clone(),
            self.config.clone(),
            self.message_sender.clone(),
            self.stats.clone(),
            requester_stats,
            self.network.clone(),
            self.ledger.clone(),
            self.block_processor_queue.clone(),
            self.bootstrap_queue.clone(),
            self.frontier_check_pool.clone(),
            self.stopped.clone(),
        );
        let join_handle = std::thread::Builder::new()
            .name("Bootstrap".to_string())
            .spawn(move || {
                requester_loop.run_loop();
            })
            .unwrap();

        *self.thread.lock().unwrap() = Some(join_handle);
    }

    pub fn stop(&self) {
        self.stopped.lock().stopped = true;
        self.stopped.notify_all();
        let thread = self.thread.lock().unwrap().take();
        if let Some(join_handle) = thread {
            join_handle.join().unwrap();
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
