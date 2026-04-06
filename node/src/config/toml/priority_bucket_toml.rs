use crate::consensus::election_schedulers::priority::PriorityBucketConfig;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct PriorityBucketToml {
    pub max_blocks: Option<usize>,
}

impl Default for PriorityBucketToml {
    fn default() -> Self {
        let config = PriorityBucketConfig::default();
        (&config).into()
    }
}

impl From<&PriorityBucketToml> for PriorityBucketConfig {
    fn from(toml: &PriorityBucketToml) -> Self {
        let mut config = PriorityBucketConfig::default();

        if let Some(max_blocks) = toml.max_blocks {
            config.max_blocks = max_blocks;
        }
        config
    }
}

impl From<&PriorityBucketConfig> for PriorityBucketToml {
    fn from(config: &PriorityBucketConfig) -> Self {
        Self {
            max_blocks: Some(config.max_blocks),
        }
    }
}
