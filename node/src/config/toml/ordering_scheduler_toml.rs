use crate::consensus::election_schedulers::OrderingSchedulerConfig;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct OrderingSchedulerToml {
    pub enable: Option<bool>,
    pub committed_threshold: Option<u32>,
}

impl Default for OrderingSchedulerToml {
    fn default() -> Self {
        let config = OrderingSchedulerConfig::new();
        Self {
            enable: Some(true),
            committed_threshold: Some(config.committed_threshold),
        }
    }
}

impl OrderingSchedulerConfig {
    pub fn merge_toml(&mut self, toml: &OrderingSchedulerToml) {
        if let Some(committed_blocks_threshold) = toml.committed_threshold {
            self.committed_threshold = committed_blocks_threshold;
        }
    }
}
