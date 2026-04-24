mod pull_count_decider;
mod pull_type_decider;
mod query_factory;
mod query_sender;
mod requester_loop;
mod stats;

use std::{
    sync::{Arc, Condvar, Mutex, RwLock},
    thread::JoinHandle,
};

use rsnano_ledger::Ledger;
use rsnano_network::Network;
use rsnano_utils::stats::{Stats, StatsCollection, StatsSource};

use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::bootstrapper::{
        BootstrapConfig,
        bootstrap_queue::BootstrapQueue,
        logic::BootstrapLogic,
        requesters::{requester_loop::RequesterLoop, stats::BootstrapRequesterStats},
    },
    transport::MessageSender,
};
pub(crate) use pull_count_decider::PullCountDecider;
pub(crate) use pull_type_decider::{PullType, PullTypeDecider};

/// Manages the threads that send out AscPullReqs
pub(crate) struct Requesters {
    config: BootstrapConfig,
    stats: Arc<Stats>,
    message_sender: MessageSender,
    state: Arc<Mutex<BootstrapLogic>>,
    state_changed: Arc<Condvar>,
    thread: Mutex<Option<JoinHandle<()>>>,
    ledger: Arc<Ledger>,
    block_processor_queue: Arc<BlockProcessorQueue>,
    bootstrap_queue: Arc<BootstrapQueue>,
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
        bootstrap_queue: Arc<BootstrapQueue>,
        network: Arc<RwLock<Network>>,
    ) -> Self {
        Self {
            config,
            stats,
            message_sender,
            state,
            state_changed,
            ledger,
            block_processor_queue,
            bootstrap_queue,
            network,
            thread: Mutex::new(None),
            stats_sources: Mutex::new(Vec::new()),
        }
    }

    pub fn start(&self) {
        let requester_stats = Arc::new(BootstrapRequesterStats::default());
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
            self.ledger.clone(),
            self.block_processor_queue.clone(),
            self.bootstrap_queue.clone(),
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
        {
            let mut state = self.state.lock().unwrap();
            state.stopped = true;
        }
        self.state_changed.notify_all();

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
