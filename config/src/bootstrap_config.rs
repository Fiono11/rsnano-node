use rsnano_messages::BlocksAckPayload;
use std::time::Duration;

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
    pub block_processor_theshold: usize,
    /** Minimum accepted protocol version used when bootstrapping */
    pub min_protocol_version: u8,
    pub max_requests: usize,
    pub optimistic_request_percentage: u8,
    pub candidate_accounts: CandidateAccountsConfig,
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
            block_processor_theshold: 1000,
            min_protocol_version: 0x14, // TODO don't hard code
            max_requests: 1024,
            optimistic_request_percentage: 75,
            candidate_accounts: Default::default(),
            frontier_scan: Default::default(),
            max_pending_frontier_responses: 16,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CandidateAccountsConfig {
    pub consideration_count: usize,
    pub priorities_max: usize,
    pub blocking_max: usize,
    pub blocking_decay: Duration,
    pub cooldown: Duration,
}

impl Default for CandidateAccountsConfig {
    fn default() -> Self {
        Self {
            consideration_count: 4,
            priorities_max: 256 * 1024,
            blocking_max: 256 * 1024,
            blocking_decay: Duration::from_secs(60 * 15),
            cooldown: Duration::from_secs(3),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrontierScanConfig {
    pub parallelism: usize,
    pub consideration_count: usize,
    pub candidates: usize,
    pub cooldown: Duration,
}

impl Default for FrontierScanConfig {
    fn default() -> Self {
        Self {
            parallelism: 128,
            consideration_count: 4,
            candidates: 1000,
            cooldown: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BootstrapServerConfig {
    pub max_queue: usize,
    pub threads: usize,
    pub batch_size: usize,
    pub limiter: usize,
}

impl Default for BootstrapServerConfig {
    fn default() -> Self {
        Self {
            max_queue: 16,
            threads: 1,
            batch_size: 64,
            limiter: 500,
        }
    }
}
