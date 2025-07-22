use std::{cmp::max, time::Duration};
use rsnano_core::Networks;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BacklogScanConfig {
    /// Control if ongoing backlog population is enabled. If not, backlog population can still be triggered by RPC
    pub enabled: bool,

    /// Number of accounts per second to process.
    pub batch_size: usize,

    /// Number of accounts to scan per second
    pub rate_limit: usize,
}

impl Default for BacklogScanConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            batch_size: 1000,
            rate_limit: 10_000,
        }
    }
}

impl BacklogScanConfig {
    pub fn wait_time(&self) -> Duration {
        let wait_time =
            Duration::from_millis(1000 / max(self.rate_limit / self.batch_size, 1) as u64 / 2);
        max(wait_time, Duration::from_millis(10))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessQueueConfig {
    // Maximum number of blocks to queue from network peers
    pub max_peer_queue: usize,

    // Maximum number of blocks to queue from system components (local RPC, bootstrap)
    pub max_system_queue: usize,

    // Higher priority gets processed more frequently
    pub priority_live: usize,
    pub priority_bootstrap: usize,
    pub priority_local: usize,
    pub priority_system: usize,
    pub batch_size: usize,
}

impl ProcessQueueConfig {}

impl Default for ProcessQueueConfig {
    fn default() -> Self {
        Self {
            max_peer_queue: 1024,
            max_system_queue: 16 * 1024,
            priority_live: 1,
            priority_bootstrap: 8,
            priority_local: 16,
            priority_system: 32,
            batch_size: 256,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoundedBacklogConfig {
    pub max_backlog: u64,
    pub batch_size: usize,
    pub scan_rate: usize,
}

impl Default for BoundedBacklogConfig {
    fn default() -> Self {
        Self {
            max_backlog: 100_000,
            batch_size: 32,
            scan_rate: 64,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalBlockBroadcasterConfig {
    pub max_size: usize,
    pub rebroadcast_interval: Duration,
    pub max_rebroadcast_interval: Duration,
    pub broadcast_rate_limit: usize,
    pub broadcast_rate_burst_ratio: f64,
    pub cleanup_interval: Duration,
}

impl LocalBlockBroadcasterConfig {
    pub fn new(network: Networks) -> Self {
        match network {
            Networks::NanoDevNetwork => Self::default_for_dev_network(),
            _ => Default::default(),
        }
    }

    fn default_for_dev_network() -> Self {
        Self {
            rebroadcast_interval: Duration::from_secs(1),
            cleanup_interval: Duration::from_secs(1),
            ..Default::default()
        }
    }
}

impl Default for LocalBlockBroadcasterConfig {
    fn default() -> Self {
        Self {
            max_size: 1024 * 8,
            rebroadcast_interval: Duration::from_secs(3),
            max_rebroadcast_interval: Duration::from_secs(60),
            broadcast_rate_limit: 32,
            broadcast_rate_burst_ratio: 3.0,
            cleanup_interval: Duration::from_secs(60),
        }
    }
}

