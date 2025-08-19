use super::GlobalConfig;
use crate::{
    block_processing::{BacklogScanConfig, ProcessQueueConfig},
    wallets::WalletsConfig,
};
use std::time::Duration;

impl From<&GlobalConfig> for ProcessQueueConfig {
    fn from(value: &GlobalConfig) -> Self {
        value.node_config.block_processor.clone()
    }
}

impl From<&GlobalConfig> for BacklogScanConfig {
    fn from(value: &GlobalConfig) -> Self {
        value.node_config.backlog_scan.clone()
    }
}

impl From<&GlobalConfig> for WalletsConfig {
    fn from(value: &GlobalConfig) -> Self {
        let node = &value.node_config;
        Self {
            preconfigured_representatives: node.preconfigured_representatives.clone(),
            password_fanout: node.password_fanout as usize,
            receive_minimum: node.receive_minimum,
            vote_minimum: node.vote_minimum,
            voting_enabled: node.enable_voting,
            cached_work_generation_delay: if value.network_params.network.is_dev_network() {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(10)
            },
            kdf_work: if value.network_params.network.is_dev_network() {
                8
            } else {
                1024 * 64
            },
        }
    }
}
