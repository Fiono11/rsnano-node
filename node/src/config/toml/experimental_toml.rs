use crate::config::NodeConfig;
use rsnano_types::Peer;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Deserialize, Serialize)]
pub struct ExperimentalToml {
    pub secondary_work_peers: Option<Vec<String>>,
}

impl NodeConfig {
    pub fn merge_experimental_toml(&mut self, toml: &ExperimentalToml) {
        if let Some(peers) = &toml.secondary_work_peers {
            self.secondary_work_peers = peers
                .iter()
                .map(|string| Peer::from_str(string).expect("Invalid secondary work peer"))
                .collect();
        }
    }
}

impl From<&NodeConfig> for ExperimentalToml {
    fn from(config: &NodeConfig) -> Self {
        Self {
            secondary_work_peers: Some(
                config
                    .secondary_work_peers
                    .iter()
                    .map(|peer| peer.to_string())
                    .collect(),
            ),
        }
    }
}
