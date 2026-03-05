use super::NodeToml;
use crate::{block_processing::BoundedBacklogConfig, config::NodeConfig};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct BoundedBacklogToml {
    pub enable: Option<bool>,
    pub batch_size: Option<usize>,
}

impl BoundedBacklogConfig {
    pub(crate) fn merge_toml(&mut self, toml: &NodeToml) {
        if let Some(max) = toml.max_backlog {
            self.max_backlog = max;
        }
        let Some(backlog_toml) = &toml.bounded_backlog else {
            return;
        };

        if let Some(size) = backlog_toml.batch_size {
            self.rollback_batch_size = size;
        }
    }
}

impl From<&NodeConfig> for BoundedBacklogToml {
    fn from(value: &NodeConfig) -> Self {
        Self {
            enable: Some(value.enable_bounded_backlog),
            batch_size: Some(value.bounded_backlog.rollback_batch_size),
        }
    }
}
