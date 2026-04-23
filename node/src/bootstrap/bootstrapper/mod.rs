pub mod logic;

mod block_inspector;
mod cleanup;
mod requesters;
mod response_processor;

pub use logic::{
    BootstrapQueueSnapshot, BootstrappingAccountInfo, FrontierHeadInfo, FrontierScanConfig,
};

use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, RwLock},
    thread::JoinHandle,
    time::Duration,
};

use tracing::{trace, warn};

use rsnano_ledger::{Ledger, LedgerEvent, LedgerSet, ProcessResult};
use rsnano_messages::{AscPullAck, BlocksAckPayload};
use rsnano_messages::{AscPullReqType, FrontiersReqPayload, HashType};
use rsnano_network::{Channel, ChannelId, DeadChannelCleanupStep, Network};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Account, BlockHash};
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, Sample, StatType, Stats, StatsCollection, StatsSource},
};

use crate::{
    block_processing::{BlockProcessorQueue, LedgerPipelineEvent},
    transport::MessageSender,
};

use block_inspector::BlockInspector;
use cleanup::BootstrapCleanup;
use logic::{
    BootstrapLogic, BootstrapQueueConfig, BootstrapQueueInfo, Priority, ProcessError, QueryType,
};
use requesters::Requesters;
use response_processor::ResponseProcessor;

#[derive(PartialEq, Eq, Debug, Clone)]
pub(crate) struct AscPullQuerySpec {
    pub query_id: u64,
    pub channel: Arc<Channel>,
    pub req_type: AscPullReqType,
    pub account: Account,
    pub hash: BlockHash,
}

impl AscPullQuerySpec {
    #[allow(dead_code)]
    pub fn new_test_instance() -> Self {
        Self {
            query_id: 123567,
            req_type: AscPullReqType::Frontiers(FrontiersReqPayload {
                start: 100.into(),
                count: 1000,
            }),
            channel: Arc::new(Channel::new_test_instance()),
            account: Account::from(100),
            hash: BlockHash::from(200),
        }
    }

    pub fn query_type(&self) -> QueryType {
        match &self.req_type {
            AscPullReqType::Blocks(b) => match b.start_type {
                HashType::Account => QueryType::BlocksByAccount,
                HashType::Block => QueryType::BlocksByHash,
            },
            AscPullReqType::AccountInfo(_) => QueryType::AccountInfoByHash,
            AscPullReqType::Frontiers(_) => QueryType::Frontiers,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BootstrapConfig {
    pub enable: bool,
    pub enable_block_requester: bool,
    pub enable_dependency_walker: bool,
    pub enable_frontier_scan: bool,
    /// Maximum number of un-responded requests per channel, should be lower or equal to bootstrap server max queue size
    pub channel_limit: usize,
    /// Limit of outgoing messages/s over all channels
    pub rate_limit: usize,
    pub database_rate_limit: usize,
    pub frontier_rate_limit: usize,
    pub database_warmup_ratio: usize,
    pub max_pull_count: u8,
    pub request_timeout: Duration,
    pub throttle_coefficient: usize,
    pub throttle_wait: Duration,
    pub block_processor_threshold: usize,
    /** Minimum accepted protocol version used when bootstrapping */
    pub min_protocol_version: u8,
    pub max_requests: usize,
    pub optimistic_request_percentage: u8,
    pub bootstrap_queue: BootstrapQueueConfig,
    pub frontier_scan: FrontierScanConfig,
    /// How many frontier acks can get queued in the processor
    pub max_pending_frontier_responses: usize,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            enable: true,
            enable_block_requester: true,
            enable_dependency_walker: true,
            enable_frontier_scan: true,
            channel_limit: 16,
            rate_limit: 500,
            database_rate_limit: 256,
            frontier_rate_limit: 8,
            database_warmup_ratio: 10,
            max_pull_count: BlocksAckPayload::MAX_BLOCKS,
            request_timeout: Duration::from_secs(15),
            throttle_coefficient: 8 * 1024,
            throttle_wait: Duration::from_millis(100),
            block_processor_threshold: 1024 * 8,
            min_protocol_version: 0x14, // TODO don't hard code
            max_requests: 1024,
            optimistic_request_percentage: 75,
            bootstrap_queue: Default::default(),
            frontier_scan: Default::default(),
            max_pending_frontier_responses: 16,
        }
    }
}

pub struct Bootstrapper {
    stats: Arc<Stats>,
    threads: Mutex<Option<Threads>>,
    logic: Arc<Mutex<BootstrapLogic>>,
    state_changed: Arc<Condvar>,
    config: BootstrapConfig,
    clock: Arc<SteadyClock>,
    response_handler: ResponseProcessor,
    block_inspector: BlockInspector,
    requesters: Requesters,
    ledger: Arc<Ledger>,
}

struct Threads {
    cleanup: JoinHandle<()>,
}

impl Bootstrapper {
    pub fn new(
        block_processor_queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        stats: Arc<Stats>,
        network: Arc<RwLock<Network>>,
        message_sender: MessageSender,
        config: BootstrapConfig,
    ) -> Self {
        Self::new_impl(
            block_processor_queue,
            ledger,
            stats,
            network,
            message_sender,
            config,
            Arc::new(SteadyClock::default()),
        )
    }

    fn new_impl(
        block_processor_queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        stats: Arc<Stats>,
        network: Arc<RwLock<Network>>,
        message_sender: MessageSender,
        config: BootstrapConfig,
        clock: Arc<SteadyClock>,
    ) -> Self {
        let logic = Arc::new(Mutex::new(BootstrapLogic::new(config.clone())));
        let state_changed = Arc::new(Condvar::new());

        let mut response_handler = ResponseProcessor::new(
            logic.clone(),
            stats.clone(),
            block_processor_queue.clone(),
            ledger.clone(),
        );
        response_handler.set_max_pending_frontiers(config.max_pending_frontier_responses);

        let block_inspector =
            BlockInspector::new(logic.clone(), ledger.clone(), block_processor_queue.clone());

        let requesters = Requesters::new(
            config.clone(),
            stats.clone(),
            message_sender.clone(),
            logic.clone(),
            state_changed.clone(),
            ledger.clone(),
            block_processor_queue,
            network,
        );

        Self {
            threads: Mutex::new(None),
            logic,
            state_changed,
            config,
            stats,
            clock,
            response_handler,
            block_inspector,
            requesters,
            ledger,
        }
    }

    pub fn new_null() -> Self {
        let block_processor_queue = Arc::new(BlockProcessorQueue::default());
        let ledger = Arc::new(Ledger::new_null());
        let stats = Arc::new(Stats::default());
        let network = Arc::new(RwLock::new(Network::new_test_instance()));
        let message_sender = MessageSender::new_null();
        let config = BootstrapConfig::default();
        let clock = Arc::new(SteadyClock::new_null());

        Self::new_impl(
            block_processor_queue,
            ledger,
            stats,
            network,
            message_sender,
            config,
            clock,
        )
    }

    pub fn stop(&self) {
        {
            let mut guard = self.logic.lock().unwrap();
            guard.stopped = true;
        }
        self.state_changed.notify_all();

        self.requesters.stop();

        let threads = self.threads.lock().unwrap().take();
        if let Some(threads) = threads {
            threads.cleanup.join().unwrap();
        }
    }

    #[deprecated = "don't access internal state!"]
    pub fn state(&self) -> MutexGuard<'_, BootstrapLogic> {
        self.logic.lock().unwrap()
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.logic.lock().unwrap().bootstrap_queue.contains(account)
    }

    pub fn enqueue(&self, account: Account) {
        self.logic
            .lock()
            .unwrap()
            .bootstrap_queue
            .priority_up_to(&account, Priority::INITIAL);
    }

    pub fn enqueue_batch(&self, accounts: impl IntoIterator<Item = Account>) {
        let logic = self.logic.lock().unwrap();
        for account in accounts {
            logic
                .bootstrap_queue
                .priority_up_to(&account, Priority::INITIAL);
        }
    }

    pub fn clear_blocked_accounts(&self) {
        let guard = self.logic.lock().unwrap();
        guard.bootstrap_queue.clear_blocked_accounts();
    }

    pub fn verify_blocked_accounts(&self) {
        tracing::info!("Verifying blocked accounts...");
        let missing_sends = self.logic.lock().unwrap().bootstrap_queue.missing_sends();
        let any = self.ledger.any();
        for block_hash in &missing_sends {
            if any.block_exists(block_hash) {
                tracing::warn!(
                    "Found send block that is still marked as missing: {}",
                    block_hash
                );
            }
        }
        tracing::info!("Blocked accounts verfied!")
    }

    pub fn print_processing(&self) {
        let processing = self.logic.lock().unwrap().bootstrap_queue.processing();
        tracing::info!("Processing blocks:");
        for hash in processing {
            tracing::info!("Processing: {}", hash);
        }
        tracing::info!("Processing blocks end");
    }

    /// Process `asc_pull_ack` message coming from network
    pub fn process(&self, message: AscPullAck, channel_id: ChannelId) {
        let now = self.clock.now();
        let query_id = message.id;
        let result = self.response_handler.process(message, channel_id, now);
        match result {
            Ok(info) => {
                trace!(query_id, ?channel_id, "Response processed");
                self.stats.inc(StatType::Bootstrap, DetailType::Reply);
                self.stats
                    .inc(StatType::BootstrapReply, info.query_type.into());
                self.stats.sample(
                    Sample::BootstrapTagDuration,
                    info.response_time.as_millis() as i64,
                    (0, self.config.request_timeout.as_millis() as i64),
                );
                self.state_changed.notify_all();
            }
            Err(error) => {
                trace!(query_id, ?channel_id, ?error, "Response processing failed");
                match error {
                    ProcessError::NoRunningQueryFound => {
                        self.stats.inc(StatType::Bootstrap, DetailType::MissingTag);
                    }
                    ProcessError::InvalidResponseType => {
                        self.stats
                            .inc(StatType::Bootstrap, DetailType::InvalidResponseType);
                    }
                    ProcessError::InvalidResponse => {
                        self.stats
                            .inc(StatType::Bootstrap, DetailType::InvalidResponse);
                    }
                }
            }
        }
    }

    fn inspect_blocks(&self, batch: &[ProcessResult]) {
        self.block_inspector.inspect(batch);
        self.state_changed.notify_all();
    }

    fn unblock_batch(&self, accounts: impl IntoIterator<Item = Account>) {
        let logic = self.logic.lock().unwrap();
        for account in accounts {
            logic.bootstrap_queue.unblock(account);
        }
    }

    fn run_timeouts(&self) {
        let mut cleanup = BootstrapCleanup::new(self.clock.clone(), self.stats.clone());
        let mut logic = self.logic.lock().unwrap();
        let mut last_sync = self.clock.now();
        while !logic.stopped {
            cleanup.cleanup(&mut logic);

            if last_sync.elapsed(self.clock.now()) >= Duration::from_mins(1) {
                cleanup.reinsert_known_dependencies(&mut logic);
                last_sync = self.clock.now();
            }

            self.state_changed.notify_all();

            logic = self
                .state_changed
                .wait_timeout_while(logic, Duration::from_secs(1), |s| !s.stopped)
                .unwrap()
                .0;
        }
    }

    pub fn queue_info(&self) -> BootstrapQueueInfo {
        self.logic.lock().unwrap().bootstrap_queue.info()
    }

    pub fn queue_snapshot(&self, limit: usize, filter: Option<Account>) -> BootstrapQueueSnapshot {
        self.logic
            .lock()
            .unwrap()
            .bootstrap_queue
            .snapshot(limit, filter)
    }
}

impl Drop for Bootstrapper {
    fn drop(&mut self) {
        // All threads must be stopped before destruction
        debug_assert!(self.threads.lock().unwrap().is_none());
    }
}

impl ContainerInfoProvider for Bootstrapper {
    fn container_info(&self) -> ContainerInfo {
        self.logic.lock().unwrap().container_info()
    }
}

impl StatsSource for Bootstrapper {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.response_handler.collect_stats(result);
        self.logic.lock().unwrap().collect_stats(result);
        self.requesters.collect_stats(result);
    }
}

pub trait BootstrapExt {
    fn start(&self);
}

impl BootstrapExt for Arc<Bootstrapper> {
    fn start(&self) {
        debug_assert!(self.threads.lock().unwrap().is_none());

        if !self.config.enable {
            warn!("Ascending bootstrap is disabled");
            return;
        }

        self.requesters.start();

        let self_l = Arc::clone(self);
        let cleanup = std::thread::Builder::new()
            .name("Bootstrap clean".to_string())
            .spawn(Box::new(move || self_l.run_timeouts()))
            .unwrap();

        *self.threads.lock().unwrap() = Some(Threads { cleanup });
    }
}

pub(crate) struct BootstrapperCleanup(pub Arc<Bootstrapper>);

impl DeadChannelCleanupStep for BootstrapperCleanup {
    fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        self.0
            .logic
            .lock()
            .unwrap()
            .scoring
            .clean_up_dead_channels(dead_channel_ids);
    }
}

impl EventHandler<LedgerPipelineEvent> for Bootstrapper {
    fn handle(&self, event: &LedgerPipelineEvent) {
        match event {
            LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(results)) => {
                self.inspect_blocks(&results);
            }
            LedgerPipelineEvent::Ledger(LedgerEvent::BlocksRolledBack(rolled_back)) => {
                self.unblock_batch(rolled_back.affected_accounts());
            }
            _ => {}
        }
    }
}
