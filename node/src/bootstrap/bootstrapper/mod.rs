use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, RwLock},
    thread::JoinHandle,
    time::Duration,
};

use tracing::{trace, warn};

use rsnano_ledger::{Ledger, LedgerSet, ProcessResult};
use rsnano_messages::{AscPullAck, BlocksAckPayload};
use rsnano_messages::{AscPullReqType, FrontiersReqPayload, HashType};
use rsnano_network::{Channel, ChannelId, DeadChannelCleanupStep, Network};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Account, BlockHash};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, Sample, StatType, Stats, StatsCollection, StatsSource},
};

use crate::{
    block_processing::BlockProcessorQueue, bootstrap::bootstrapper::state::Priority,
    transport::MessageSender,
};

mod block_inspector;
mod cleanup;
mod requesters;
mod response_processor;
pub mod state;

use block_inspector::BlockInspector;
use cleanup::BootstrapCleanup;
use requesters::Requesters;
use response_processor::ResponseProcessor;
use state::{BootstrapLogic, BootstrapQueueConfig};
use state::{PrioritySetResult, bootstrap_logic::ProcessError};

use state::QueryType;
pub use state::{FrontierHeadInfo, FrontierScanConfig};

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct AscPullQuerySpec {
    pub query_id: u64,
    pub channel: Arc<Channel>,
    pub req_type: AscPullReqType,
    pub account: Account,
    pub hash: BlockHash,
    pub cooldown_account: bool,
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
            cooldown_account: false,
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
    pub enable_priorities: bool,
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
            enable_priorities: true,
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
            block_processor_threshold: 1000,
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
    ledger: Arc<Ledger>,
    logic: Arc<Mutex<BootstrapLogic>>,
    state_changed: Arc<Condvar>,
    config: BootstrapConfig,
    clock: Arc<SteadyClock>,
    response_handler: ResponseProcessor,
    block_inspector: BlockInspector,
    requesters: Requesters,
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
        let state = Arc::new(Mutex::new(BootstrapLogic::new(config.clone())));
        let state_changed = Arc::new(Condvar::new());

        let mut response_handler = ResponseProcessor::new(
            state.clone(),
            stats.clone(),
            block_processor_queue.clone(),
            ledger.clone(),
        );
        response_handler.set_max_pending_frontiers(config.max_pending_frontier_responses);

        let block_inspector = BlockInspector::new(
            state.clone(),
            ledger.clone(),
            stats.clone(),
            clock.clone(),
            block_processor_queue.clone(),
        );

        let requesters = Requesters::new(
            config.clone(),
            stats.clone(),
            message_sender.clone(),
            state.clone(),
            state_changed.clone(),
            ledger.clone(),
            block_processor_queue,
            network,
        );

        Self {
            threads: Mutex::new(None),
            logic: state,
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

    pub fn initialize(&self, genesis_account: &Account) {
        self.logic
            .lock()
            .unwrap()
            .bootstrap_queue
            .priority_set(genesis_account, Priority::INITIAL);

        self.priority_inserted()
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

    pub fn inspect_blocks(&self, batch: &[ProcessResult]) {
        self.block_inspector.inspect(batch);

        self.state_changed.notify_all();
    }

    pub fn unblock_batch(&self, accounts: impl IntoIterator<Item = Account>) {
        let mut guard = self.logic.lock().unwrap();
        for account in accounts {
            guard.bootstrap_queue.unblock(account, None);
        }
    }

    pub fn enqueue(&self, account: Account) {
        let mut guard = self.logic.lock().unwrap();
        guard.bootstrap_queue.priority_up(&account);
    }

    pub fn enqueue_batch(&self, accounts: impl IntoIterator<Item = Account>) {
        let mut guard = self.logic.lock().unwrap();
        for account in accounts {
            guard.bootstrap_queue.priority_up(&account);
        }
    }

    pub fn clear_blocked_accounts(&self) {
        let mut guard = self.logic.lock().unwrap();
        guard.bootstrap_queue.clear_blocked_accounts();
    }

    pub fn consistency_check(&self) {
        tracing::info!("Performing blocked accounts consistency check...");
        let guard = self.logic.lock().unwrap();
        let any = self.ledger.any();
        for (account, dep_block, _) in guard.bootstrap_queue.iter_blocked() {
            if any.block_exists(&dep_block) {
                tracing::warn!(account = account.encode_account(), dependency = ?dep_block, "Dependency block found, but account still blocked");
            }
        }
        tracing::info!("Blocked accounts consistency check completed");
    }

    fn run_timeouts(&self) {
        let mut cleanup = BootstrapCleanup::new(self.clock.clone(), self.stats.clone());
        let mut state = self.logic.lock().unwrap();
        let mut last_sync = self.clock.now();
        while !state.stopped {
            cleanup.cleanup(&mut state);

            if last_sync.elapsed(self.clock.now()) >= Duration::from_mins(1) {
                cleanup.reinsert_known_dependencies(&mut state);
                last_sync = self.clock.now();
            }

            self.state_changed.notify_all();

            state = self
                .state_changed
                .wait_timeout_while(state, Duration::from_secs(1), |s| !s.stopped)
                .unwrap()
                .0;
        }
    }

    fn priority_inserted(&self) {
        self.stats
            .inc(StatType::BootstrapAccountSets, DetailType::PriorityInsert);
    }

    fn priority_insertion_failed(&self) {
        self.stats
            .inc(StatType::BootstrapAccountSets, DetailType::PrioritizeFailed);
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
