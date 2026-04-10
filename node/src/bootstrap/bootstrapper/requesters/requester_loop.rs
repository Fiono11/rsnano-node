use std::{
    sync::{Arc, Condvar, Mutex, RwLock, atomic::Ordering::Relaxed},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_network::Network;
use rsnano_utils::stats::Stats;

use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::bootstrapper::{
        BootstrapConfig,
        requesters::{query_sender::QuerySender, query_spec_factory::QuerySpecFactory},
        state::BootstrapLogic,
    },
    transport::MessageSender,
};

use super::stats::BootstrapRequesterStats;

pub(super) struct RequesterLoop {
    state: Arc<Mutex<BootstrapLogic>>,
    state_changed: Arc<Condvar>,
    config: BootstrapConfig,
    query_sender: QuerySender,
    query_spec_factory: QuerySpecFactory,
    stats: Arc<BootstrapRequesterStats>,
}

impl RequesterLoop {
    const THROTTLE_WAIT: Duration = Duration::from_millis(50);

    pub(super) fn new(
        logic: Arc<Mutex<BootstrapLogic>>,
        state_changed: Arc<Condvar>,
        config: BootstrapConfig,
        message_sender: MessageSender,
        stats: Arc<Stats>,
        stats2: Arc<BootstrapRequesterStats>,
        network: Arc<RwLock<Network>>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
    ) -> Self {
        let mut query_sender = QuerySender::new(message_sender, stats.clone());
        query_sender.set_request_timeout(config.request_timeout);

        Self {
            state: logic,
            state_changed,
            config: config.clone(),
            query_sender,
            stats: stats2.clone(),
            query_spec_factory: QuerySpecFactory::new(
                config,
                stats2,
                network,
                ledger,
                block_processor_queue,
            ),
        }
    }

    pub fn run_loop(&mut self) {
        let mut state = self.state.lock().unwrap();
        let mut loop_counter = 0;
        while !state.stopped {
            let mut produced = 0;

            if self.config.enable_priorities
                && let Some(spec) = self.query_spec_factory.try_priority_query(&mut state)
            {
                self.query_sender.send(spec, &mut state);
                produced += 1;
            }
            if self.config.enable_dependency_walker
                && let Some(spec) = self.query_spec_factory.try_dependency_query(&mut state)
            {
                self.query_sender.send(spec, &mut state);
                produced += 1;
            }
            if self.config.enable_frontier_scan
                && let Some(spec) = self.query_spec_factory.try_frontier_query(&mut state)
            {
                self.query_sender.send(spec, &mut state);
                produced += 1;
            }

            if produced == 0 {
                self.stats.sleep.fetch_add(1, Relaxed);
                loop_counter = 0;
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
