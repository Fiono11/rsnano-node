use super::GlobalConfig;
use crate::{BacklogScanConfig, ProcessQueueConfig};

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
