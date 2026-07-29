use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::RaiConfig;
use rsnano_types::{Account, PublicKey};

#[derive(Deserialize, Serialize, Default)]
pub struct RaiToml {
    pub epoch_duration: Option<u64>,
    pub close_attempt_duration: Option<u64>,
    pub tick_interval: Option<u64>,
    pub genesis_committee: Option<Vec<String>>,
}

impl RaiConfig {
    pub fn merge_toml(&mut self, toml: &RaiToml) {
        if let Some(epoch_duration) = toml.epoch_duration {
            self.epoch_duration = Duration::from_millis(epoch_duration);
        }
        if let Some(close_attempt_duration) = toml.close_attempt_duration {
            self.close_attempt_duration = Duration::from_millis(close_attempt_duration);
        }
        if let Some(tick_interval) = toml.tick_interval {
            self.tick_interval = Duration::from_millis(tick_interval);
        }
        if let Some(genesis_committee) = &toml.genesis_committee {
            self.genesis_committee = genesis_committee
                .iter()
                .map(|value| {
                    PublicKey::from(
                        Account::parse(value).expect("Invalid RAI genesis committee member"),
                    )
                })
                .collect();
        }
    }
}

impl From<&RaiConfig> for RaiToml {
    fn from(config: &RaiConfig) -> Self {
        Self {
            epoch_duration: Some(config.epoch_duration.as_millis() as u64),
            close_attempt_duration: Some(config.close_attempt_duration.as_millis() as u64),
            tick_interval: Some(config.tick_interval.as_millis() as u64),
            genesis_committee: (!config.genesis_committee.is_empty()).then(|| {
                config
                    .genesis_committee
                    .iter()
                    .map(|key| Account::from(key).encode_account())
                    .collect()
            }),
        }
    }
}
