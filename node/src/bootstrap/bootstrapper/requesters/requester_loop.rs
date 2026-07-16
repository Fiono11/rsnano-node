use std::{
    sync::{Arc, RwLock, atomic::Ordering::Relaxed},
    time::Duration,
};

use rsnano_ledger::Ledger;
use rsnano_network::Network;
use rsnano_utils::stats::Stats;

#[cfg(feature = "rai_protocol")]
use crate::bootstrap::bootstrapper::RaiEpochBootstrap;
use crate::{
    bootstrap::bootstrapper::{
        BootstrapConfig, StoppedFlag,
        bootstrap_queue::BootstrapQueue,
        frontier_scan::FrontierScan,
        peer_scoring::PeerScoring,
        query_tracker::QueryTracker,
        requesters::{query_factory::QueryFactory, query_sender::QuerySender},
    },
    transport::MessageSender,
};

use super::stats::BootstrapRequesterStats;
use rsnano_nullable_condvar::NullableCondvarMutex;

pub(super) struct RequesterLoop {
    query_sender: QuerySender,
    query_factory: QueryFactory,
    bootstrap_queue: Arc<BootstrapQueue>,
    stats: Arc<BootstrapRequesterStats>,
    stopped: Arc<NullableCondvarMutex<StoppedFlag>>,
}

impl RequesterLoop {
    const THROTTLE_WAIT: Duration = Duration::from_millis(50);

    pub(super) fn new(
        query_tracker: Arc<QueryTracker>,
        peer_scoring: Arc<PeerScoring>,
        config: BootstrapConfig,
        message_sender: MessageSender,
        stats: Arc<Stats>,
        stats2: Arc<BootstrapRequesterStats>,
        network: Arc<RwLock<Network>>,
        ledger: Arc<Ledger>,
        bootstrap_queue: Arc<BootstrapQueue>,
        frontier_scan: Arc<FrontierScan>,
        stopped: Arc<NullableCondvarMutex<StoppedFlag>>,
        #[cfg(feature = "rai_protocol")] rai_epoch_bootstrap: Arc<
            std::sync::Mutex<Option<Arc<RaiEpochBootstrap>>>,
        >,
    ) -> Self {
        let mut query_sender = QuerySender::new(
            message_sender,
            stats.clone(),
            query_tracker.clone(),
            peer_scoring.clone(),
        );
        query_sender.set_request_timeout(config.request_timeout);

        Self {
            query_sender,
            stats: stats2.clone(),
            query_factory: QueryFactory::new(
                config,
                stats2,
                network,
                ledger,
                bootstrap_queue.clone(),
                frontier_scan,
                query_tracker,
                peer_scoring,
                #[cfg(feature = "rai_protocol")]
                rai_epoch_bootstrap,
            ),
            bootstrap_queue,
            stopped,
        }
    }

    pub fn run_loop(&mut self) {
        let mut stopped = self.stopped.lock();
        let mut last_revision;
        while !stopped.stopped {
            drop(stopped);

            let sent = if let Some(spec) = self.query_factory.try_query() {
                let sent = self.query_sender.send(&spec);
                if !sent {
                    self.query_factory.query_failed(&spec);
                }
                sent
            } else {
                false
            };

            if !sent {
                // nothing to do — wait for a state change or fixed throttle
                self.stats.sleep.fetch_add(1, Relaxed);
                last_revision = self.bootstrap_queue.revision();
                stopped = self.stopped.lock();
                stopped = self
                    .stopped
                    .wait_timeout_while(stopped, Self::THROTTLE_WAIT, |s| {
                        self.bootstrap_queue.revision() == last_revision && !s.stopped
                    })
                    .0;
            } else {
                stopped = self.stopped.lock();
            }
        }
    }
}
