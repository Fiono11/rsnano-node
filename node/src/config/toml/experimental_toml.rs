use crate::config::NodeConfig;
use rsnano_types::Peer;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Deserialize, Serialize)]
pub struct ExperimentalToml {
    pub secondary_work_peers: Option<Vec<String>>,
    pub rai_epoch_duration_seconds: Option<u64>,
    pub rai_epoch_duration_ms: Option<u64>,
}

impl NodeConfig {
    pub fn merge_experimental_toml(&mut self, toml: &ExperimentalToml) {
        if let Some(peers) = &toml.secondary_work_peers {
            self.secondary_work_peers = peers
                .iter()
                .map(|string| Peer::from_str(string).expect("Invalid secondary work peer"))
                .collect();
        }
        if let Some(duration_s) = toml.rai_epoch_duration_seconds {
            self.rai_epoch_duration = std::time::Duration::from_secs(duration_s);
        } else if let Some(duration_ms) = toml.rai_epoch_duration_ms {
            self.rai_epoch_duration = std::time::Duration::from_millis(duration_ms);
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
            rai_epoch_duration_seconds: Some(config.rai_epoch_duration.as_secs()),
            rai_epoch_duration_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::NetworkType;
    use std::time::Duration;

    #[test]
    fn merge_prefers_epoch_duration_seconds() {
        let mut config = NodeConfig::default_for(NetworkType::NanoDevNetwork, 1);

        config.merge_experimental_toml(&ExperimentalToml {
            secondary_work_peers: None,
            rai_epoch_duration_seconds: Some(7),
            rai_epoch_duration_ms: Some(250),
        });

        assert_eq!(config.rai_epoch_duration, Duration::from_secs(7));
    }

    #[test]
    fn writes_epoch_duration_in_seconds() {
        let mut config = NodeConfig::default_for(NetworkType::NanoDevNetwork, 1);
        config.rai_epoch_duration = Duration::from_secs(9);

        let toml = ExperimentalToml::from(&config);

        assert_eq!(toml.rai_epoch_duration_seconds, Some(9));
        assert_eq!(toml.rai_epoch_duration_ms, None);
    }
}
