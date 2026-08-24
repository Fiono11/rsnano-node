use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    time::Instant,
};

use rsnano_types::NetworkType;
use rsnano_utils::{
    BackpressureHandlerRegistry, EventHandlerMut, EventHandlerRegistry,
    stats::{StatsCollection, StatsSource},
};

use crate::{
    NodeEvent,
    block_processing::{BlockProcessorQueue, LedgerPipelineEvent},
    cementation::{ConfirmingSet, ConfirmingSetEvent},
    consensus::{
        AecCooldownReason, AecService, DependentElectionsConfirmer, ForkCache, ForkCacheUpdater,
        LocalVoteHistory,
    },
    utils::BackpressureEventProcessor,
};
use rsnano_ledger::LedgerEvent;

pub(crate) struct LedgerEventProcessor {
    pub(crate) node_event_sender: Option<SyncSender<NodeEvent>>,
    pub confirming_set: Arc<ConfirmingSet>,
    pub stats: Arc<LedgerEventProcessorStats>,
    pub(crate) dependent_elections_confirmer: DependentElectionsConfirmer,
    pub(crate) vote_history: Arc<LocalVoteHistory>,
    pub(crate) active_elections: Arc<AecService>,
    pub(crate) block_processor_queue: Arc<BlockProcessorQueue>,
    pub(crate) fork_cache_updater: ForkCacheUpdater,
    pub(crate) plugins: EventHandlerRegistry<LedgerPipelineEvent>,
    pub(crate) backpressure_plugins: BackpressureHandlerRegistry,
}

impl LedgerEventProcessor {
    #[allow(dead_code)]
    pub fn new_null() -> Self {
        Self {
            node_event_sender: None,
            confirming_set: Arc::new(ConfirmingSet::new_null()),
            stats: Arc::new(Default::default()),
            dependent_elections_confirmer: DependentElectionsConfirmer::new_null(),
            vote_history: Arc::new(LocalVoteHistory::new(NetworkType::NanoLiveNetwork)),
            active_elections: Arc::new(AecService::new_null()),
            block_processor_queue: Arc::new(BlockProcessorQueue::default()),
            fork_cache_updater: ForkCacheUpdater::new(Arc::new(RwLock::new(ForkCache::default()))),
            plugins: EventHandlerRegistry::default(),
            backpressure_plugins: BackpressureHandlerRegistry::default(),
        }
    }
}

impl BackpressureEventProcessor<LedgerPipelineEvent> for LedgerEventProcessor {
    fn cool_down(&mut self) {
        self.backpressure_plugins.cool_down();
        self.confirming_set.set_cooldown(true);
        self.block_processor_queue.set_cooldown(true);
        self.stats.cool_down.fetch_add(1, Ordering::Relaxed);
    }

    fn recovered(&mut self) {
        self.backpressure_plugins.recovered();
        self.confirming_set.set_cooldown(false);
        self.block_processor_queue.set_cooldown(false);
        self.stats.recovered.fetch_add(1, Ordering::Relaxed);
    }

    fn process(&mut self, event: LedgerPipelineEvent) {
        let start = Instant::now();
        let duration_stat = match &event {
            LedgerPipelineEvent::Ledger(e) => match e {
                LedgerEvent::BlocksProcessed(_) => &self.stats.dur_blocks_processed,
                LedgerEvent::BlocksConfirmed(_) => &self.stats.dur_blocks_confirmed,
                LedgerEvent::BlocksRolledBack(_) => &self.stats.dur_blocks_rolled_back,
            },
            LedgerPipelineEvent::ConfirmingSet(e) => match e {
                ConfirmingSetEvent::ConfirmationFailed(_) => &self.stats.dur_confirmation_failed,
                ConfirmingSetEvent::NearFull => &self.stats.dur_conf_set_near_full,
                ConfirmingSetEvent::Recovered => &self.stats.dur_conf_set_recovered,
            },
        };

        self.stats.processed.fetch_add(1, Ordering::Relaxed);
        self.plugins.handle(&event);

        match event {
            LedgerPipelineEvent::Ledger(event) => match event {
                LedgerEvent::BlocksProcessed(results) => {
                    self.stats
                        .ev_blocks_processed
                        .fetch_add(1, Ordering::Relaxed);
                    self.stats
                        .ev_blocks_processed_total
                        .fetch_add(results.len() as u64, Ordering::Relaxed);

                    self.confirming_set.requeue_blocks(&results);
                    self.fork_cache_updater.update(&results);
                    if let Some(sender) = &self.node_event_sender {
                        sender.send(NodeEvent::BlocksProcessed(results)).unwrap();
                    }
                }
                LedgerEvent::BlocksConfirmed(confirmed) => {
                    self.stats
                        .ev_blocks_confirmed
                        .fetch_add(1, Ordering::Relaxed);
                    self.stats
                        .ev_blocks_confirmed_total
                        .fetch_add(confirmed.len() as u64, Ordering::Relaxed);
                    let dep_conf_start = Instant::now();
                    self.dependent_elections_confirmer
                        .confirm_dependent_elections(&confirmed);
                    #[cfg(feature = "rai_protocol")]
                    for (block, _) in &confirmed {
                        self.active_elections.apply_cemented_outcome(block);
                    }
                    self.stats.dur_dependent_elections.fetch_add(
                        dep_conf_start.elapsed().as_millis() as u64,
                        Ordering::Relaxed,
                    );
                }
                LedgerEvent::BlocksRolledBack(rolled_back) => {
                    self.stats
                        .ev_blocks_rolled_back
                        .fetch_add(1, Ordering::Relaxed);
                    self.stats
                        .ev_blocks_rolled_back_total
                        .fetch_add(rolled_back.len() as u64, Ordering::Relaxed);
                    {
                        for result in rolled_back.iter() {
                            for block in &result.rolled_back {
                                // Stop all rolled back elections except initial
                                if block.qualified_root() != result.target_root {
                                    #[cfg(not(feature = "rai_protocol"))]
                                    self.active_elections.erase(&block.qualified_root());
                                    #[cfg(feature = "rai_protocol")]
                                    self.active_elections.apply_rolled_back_block(&block.hash());
                                }
                            }
                        }
                    }

                    self.vote_history.erase_batch(rolled_back.roots());
                }
            },
            LedgerPipelineEvent::ConfirmingSet(event) => match event {
                ConfirmingSetEvent::ConfirmationFailed(hash) => {
                    self.stats
                        .ev_confirmation_failed
                        .fetch_add(1, Ordering::Relaxed);
                    // The block never got confirmed! Clean up the election, so
                    // that a new election for this block can be started
                    self.active_elections.remove_recently_confirmed(&hash);
                }
                ConfirmingSetEvent::NearFull => {
                    self.stats
                        .ev_conf_set_near_full
                        .fetch_add(1, Ordering::Relaxed);
                    self.active_elections
                        .set_cooldown(true, AecCooldownReason::ConfirmingSetFull);
                }
                ConfirmingSetEvent::Recovered => {
                    self.stats
                        .ev_conf_set_recovered
                        .fetch_add(1, Ordering::Relaxed);
                    self.active_elections
                        .set_cooldown(false, AecCooldownReason::ConfirmingSetFull);
                }
            },
        }

        let elapsed = start.elapsed().as_millis() as u64;
        duration_stat.fetch_add(elapsed, Ordering::Relaxed);
        self.stats
            .dur_total
            .fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(crate) struct LedgerEventProcessorStats {
    processed: AtomicU64,
    cool_down: AtomicU64,
    recovered: AtomicU64,

    ev_blocks_processed: AtomicU64,
    ev_blocks_processed_total: AtomicU64,
    ev_blocks_confirmed: AtomicU64,
    ev_blocks_confirmed_total: AtomicU64,
    ev_blocks_rolled_back: AtomicU64,
    ev_blocks_rolled_back_total: AtomicU64,
    ev_confirmation_failed: AtomicU64,
    ev_conf_set_near_full: AtomicU64,
    ev_conf_set_recovered: AtomicU64,

    dur_total: AtomicU64,
    dur_blocks_processed: AtomicU64,
    dur_blocks_confirmed: AtomicU64,
    dur_blocks_rolled_back: AtomicU64,
    dur_confirmation_failed: AtomicU64,
    dur_conf_set_near_full: AtomicU64,
    dur_conf_set_recovered: AtomicU64,
    dur_dependent_elections: AtomicU64,
    dur_activate_backlog: AtomicU64,
}

impl StatsSource for LedgerEventProcessorStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const KEY: &'static str = "ledger_ev_proc";
        result.insert(KEY, "processed", self.processed.load(Ordering::Relaxed));
        result.insert(KEY, "cool_down", self.cool_down.load(Ordering::Relaxed));
        result.insert(KEY, "recovered", self.recovered.load(Ordering::Relaxed));
        result.insert(
            KEY,
            "ev_blocks_processed",
            self.ev_blocks_processed.load(Ordering::Relaxed),
        );
        result.insert(
            KEY,
            "ev_blocks_processed_total",
            self.ev_blocks_processed_total.load(Ordering::Relaxed),
        );
        result.insert(
            KEY,
            "ev_blocks_confirmed",
            self.ev_blocks_confirmed.load(Ordering::Relaxed),
        );
        result.insert(
            KEY,
            "ev_blocks_confirmed_total",
            self.ev_blocks_confirmed_total.load(Ordering::Relaxed),
        );
        result.insert(
            KEY,
            "ev_blocks_rolled_back",
            self.ev_blocks_rolled_back.load(Ordering::Relaxed),
        );
        result.insert(
            KEY,
            "ev_blocks_rolled_back_total",
            self.ev_blocks_rolled_back_total.load(Ordering::Relaxed),
        );
        result.insert(
            KEY,
            "ev_conf_set_near_full",
            self.ev_conf_set_near_full.load(Ordering::Relaxed),
        );
        result.insert(
            KEY,
            "ev_conf_set_recovered",
            self.ev_conf_set_recovered.load(Ordering::Relaxed),
        );

        const DUR_KEY: &'static str = "ledger_ev_dur";
        result.insert(
            "event_proc_duration",
            "ledger_ev_queue",
            self.dur_total.load(Ordering::Relaxed),
        );
        result.insert(
            DUR_KEY,
            "dur_blocks_processed",
            self.dur_blocks_processed.load(Ordering::Relaxed),
        );
        result.insert(
            DUR_KEY,
            "dur_blocks_confirmed",
            self.dur_blocks_confirmed.load(Ordering::Relaxed),
        );
        result.insert(
            DUR_KEY,
            "dur_blocks_rolled_back",
            self.dur_blocks_rolled_back.load(Ordering::Relaxed),
        );
        result.insert(
            DUR_KEY,
            "dur_confirmation_failed",
            self.dur_confirmation_failed.load(Ordering::Relaxed),
        );
        result.insert(
            DUR_KEY,
            "dur_conf_set_near_full",
            self.dur_conf_set_near_full.load(Ordering::Relaxed),
        );
        result.insert(
            DUR_KEY,
            "dur_conf_set_recovered",
            self.dur_conf_set_recovered.load(Ordering::Relaxed),
        );
        result.insert(
            DUR_KEY,
            "dur_dependent_elections",
            self.dur_dependent_elections.load(Ordering::Relaxed),
        );
        result.insert(
            DUR_KEY,
            "dur_activate_backlog",
            self.dur_activate_backlog.load(Ordering::Relaxed),
        );
    }
}
