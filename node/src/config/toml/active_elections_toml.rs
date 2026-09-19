use crate::config::NodeConfig;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Default)]
pub struct ActiveElectionsToml {
    pub confirmation_cache: Option<usize>,
    pub confirmation_history_size: Option<usize>,
    pub hinted_limit_percentage: Option<usize>,
    pub optimistic_limit_percentage: Option<usize>,
    pub size: Option<usize>,
    pub bootstrap_stale_threshold: Option<usize>,
    /// RAI: advance the consensus epoch after this many decided elections; 0 never
    pub epoch_terminated_elections: Option<usize>,
    /// RAI: end an epoch this long after its first election; 0 never
    pub epoch_duration_ms: Option<u64>,
    /// RAI: a replica abstains in a round of an epoch's close election after
    /// waiting this long for a valid proposal
    pub close_round_timeout_ms: Option<u64>,
}

impl From<&NodeConfig> for ActiveElectionsToml {
    fn from(config: &NodeConfig) -> Self {
        Self {
            size: Some(config.active_elections.max_elections),
            hinted_limit_percentage: Some(config.hinted_scheduler.hinted_limit_percentage),
            optimistic_limit_percentage: Some(
                config.optimistic_scheduler.optimistic_limit_percentage,
            ),
            confirmation_history_size: Some(config.confirmation_history_size),
            confirmation_cache: Some(config.active_elections.confirmation_cache),
            bootstrap_stale_threshold: Some(config.bootstrap_stale_threshold.as_secs() as usize),
            epoch_terminated_elections: Some(config.active_elections.epoch_terminated_elections),
            epoch_duration_ms: Some(config.active_elections.epoch_duration.as_millis() as u64),
            close_round_timeout_ms: Some(
                config.active_elections.close_round_timeout.as_millis() as u64
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn convert_from_node_config() {
        let config = NodeConfig {
            bootstrap_stale_threshold: Duration::from_secs(42),
            ..NodeConfig::new_test_instance()
        };
        let toml = ActiveElectionsToml::from(&config);
        assert_eq!(
            toml.confirmation_cache,
            Some(config.active_elections.confirmation_cache)
        );
        assert_eq!(toml.bootstrap_stale_threshold, Some(42));
        assert_eq!(toml.epoch_terminated_elections, Some(0));
        assert_eq!(toml.epoch_duration_ms, Some(0));
        assert_eq!(toml.close_round_timeout_ms, Some(5000));
    }
}
