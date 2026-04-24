use std::{
    sync::{Arc, Mutex, RwLock, atomic::Ordering::Relaxed},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_network::Network;
use rsnano_utils::stats::Stats;

use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::bootstrapper::{
        BootstrapConfig, StoppedFlag,
        bootstrap_queue::BootstrapQueue,
        frontier_scan::frontiers_processor::FrontiersProcessor,
        query_tracker::QueryTracker,
        requesters::{query_factory::QueryFactory, query_sender::QuerySender},
    },
    transport::MessageSender,
};

use super::stats::BootstrapRequesterStats;
use rsnano_nullable_condvar::NullableCondvarMutex;

pub(super) struct RequesterLoop {
    query_tracker: Arc<Mutex<QueryTracker>>,
    config: BootstrapConfig,
    query_sender: QuerySender,
    query_spec_factory: QueryFactory,
    bootstrap_queue: Arc<BootstrapQueue>,
    stats: Arc<BootstrapRequesterStats>,
    stopped: Arc<NullableCondvarMutex<StoppedFlag>>,
}

impl RequesterLoop {
    const THROTTLE_WAIT: Duration = Duration::from_millis(50);

    pub(super) fn new(
        query_tracker: Arc<Mutex<QueryTracker>>,
        config: BootstrapConfig,
        message_sender: MessageSender,
        stats: Arc<Stats>,
        stats2: Arc<BootstrapRequesterStats>,
        network: Arc<RwLock<Network>>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
        bootstrap_queue: Arc<BootstrapQueue>,
        frontiers_processor: Arc<FrontiersProcessor>,
        stopped: Arc<NullableCondvarMutex<StoppedFlag>>,
    ) -> Self {
        let mut query_sender = QuerySender::new(message_sender, stats.clone());
        query_sender.set_request_timeout(config.request_timeout);

        Self {
            query_tracker,
            config: config.clone(),
            query_sender,
            stats: stats2.clone(),
            query_spec_factory: QueryFactory::new(
                config,
                stats2,
                network,
                ledger,
                block_processor_queue,
                bootstrap_queue.clone(),
                frontiers_processor,
            ),
            bootstrap_queue,
            stopped,
        }
    }

    pub fn run_loop(&mut self) {
        let mut stopped = self.stopped.lock();
        let mut loop_counter = 0;
        let mut last_revision;
        while !stopped.stopped {
            let mut state = self.query_tracker.lock().unwrap();
            let sent = if self.config.enable_block_requester
                && let Some(spec) = self.query_spec_factory.try_blocks_query(&mut state)
            {
                self.query_sender.send(spec, &mut state)
            } else if self.config.enable_dependency_walker
                && let Some(spec) = self.query_spec_factory.try_dependency_query(&mut state)
            {
                self.query_sender.send(spec, &mut state)
            } else if self.config.enable_frontier_scan
                && let Some(spec) = self.query_spec_factory.try_frontier_query(&mut state)
            {
                self.query_sender.send(spec, &mut state)
            } else {
                false
            };
            drop(state);

            if !sent {
                self.stats.sleep.fetch_add(1, Relaxed);
                loop_counter = 0;
                // nothing to do — wait for a state change or fixed throttle
                last_revision = self.bootstrap_queue.revision();

                stopped = self
                    .stopped
                    .wait_timeout_while(stopped, Self::THROTTLE_WAIT, |s| {
                        self.bootstrap_queue.revision() == last_revision && !s.stopped
                    })
                    .0;
            } else {
                loop_counter += 1;
                if loop_counter > 0 {
                    loop_counter = 0;
                    // periodically release the lock so cleanup/response threads can run
                    drop(stopped);
                    std::thread::yield_now();
                    stopped = self.stopped.lock();
                }
            }
        }
    }
}
