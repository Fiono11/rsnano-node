use serde::{Deserialize, Serialize};

use crate::{
    config::NodeConfig,
    consensus::{SigningSync, election::CommitteeModel},
};

/// Reject misspelled models during config parsing instead of silently changing quorum rules.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommitteeModelToml {
    Weighted,
    EqualWeight,
    BoundedWeight,
}

/// RAI: how the signing records reach the disk (see `SigningSync`)
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SigningSyncToml {
    None,
    Fsync,
    Full,
}

impl From<SigningSyncToml> for SigningSync {
    fn from(value: SigningSyncToml) -> Self {
        match value {
            SigningSyncToml::None => SigningSync::None,
            SigningSyncToml::Fsync => SigningSync::Fsync,
            SigningSyncToml::Full => SigningSync::Full,
        }
    }
}

impl From<SigningSync> for SigningSyncToml {
    fn from(value: SigningSync) -> Self {
        match value {
            SigningSync::None => SigningSyncToml::None,
            SigningSync::Fsync => SigningSyncToml::Fsync,
            SigningSync::Full => SigningSyncToml::Full,
        }
    }
}

#[derive(Deserialize, Serialize, Default)]
pub struct ActiveElectionsToml {
    pub confirmation_cache: Option<usize>,
    pub confirmation_history_size: Option<usize>,
    pub hinted_limit_percentage: Option<usize>,
    pub optimistic_limit_percentage: Option<usize>,
    pub size: Option<usize>,
    pub bootstrap_stale_threshold: Option<usize>,
    /// RAI: end an epoch this long after its first election; 0 never
    pub epoch_duration_ms: Option<u64>,
    /// RAI: a replica abstains in a round of an epoch's close election after
    /// waiting this long for a valid proposal
    pub close_round_timeout_ms: Option<u64>,
    /// RAI: whether this node casts account votes; false makes a silent
    /// representative, which reports and votes in the close only
    pub account_voting: Option<bool>,
    pub committee_model: Option<CommitteeModelToml>,
    pub committee_f: Option<u32>,
    pub committee_p: Option<u32>,
    /// RAI bounded_weight: how far a member's weight may move from the equal
    /// share, in permille of it
    pub committee_drift: Option<u32>,
    /// RAI evaluation: append every signed vote to `signed-votes.log`
    pub signed_vote_log: Option<bool>,
    /// RAI: "none", "fsync" (default) or "full"
    pub signing_sync: Option<SigningSyncToml>,
}

impl From<&NodeConfig> for ActiveElectionsToml {
    fn from(config: &NodeConfig) -> Self {
        let model = config.active_elections.committee_model;
        Self {
            size: Some(config.active_elections.max_elections),
            hinted_limit_percentage: Some(config.hinted_scheduler.hinted_limit_percentage),
            optimistic_limit_percentage: Some(
                config.optimistic_scheduler.optimistic_limit_percentage,
            ),
            confirmation_history_size: Some(config.confirmation_history_size),
            confirmation_cache: Some(config.active_elections.confirmation_cache),
            bootstrap_stale_threshold: Some(config.bootstrap_stale_threshold.as_secs() as usize),
            epoch_duration_ms: Some(config.active_elections.epoch_duration.as_millis() as u64),
            close_round_timeout_ms: Some(
                config.active_elections.close_round_timeout.as_millis() as u64
            ),
            account_voting: Some(config.active_elections.account_voting),
            committee_model: Some(match model {
                CommitteeModel::Weighted => CommitteeModelToml::Weighted,
                CommitteeModel::EqualWeight { .. } => CommitteeModelToml::EqualWeight,
                CommitteeModel::BoundedWeight { .. } => CommitteeModelToml::BoundedWeight,
            }),
            committee_f: match model {
                CommitteeModel::EqualWeight { f, .. } | CommitteeModel::BoundedWeight { f, .. } => {
                    Some(f)
                }
                CommitteeModel::Weighted => None,
            },
            committee_p: match model {
                CommitteeModel::EqualWeight { p, .. } | CommitteeModel::BoundedWeight { p, .. } => {
                    Some(p)
                }
                CommitteeModel::Weighted => None,
            },
            committee_drift: match model {
                CommitteeModel::BoundedWeight { drift, .. } => Some(drift),
                _ => None,
            },
            signed_vote_log: Some(config.active_elections.signed_vote_log),
            signing_sync: Some(config.active_elections.signing_sync.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn equal_weight_config_round_trip_and_unknown_model_rejection() {
        let mut config = NodeConfig::new_test_instance();
        config.active_elections.committee_model = CommitteeModel::EqualWeight { f: 2, p: 1 };
        let encoded = toml::to_string(&ActiveElectionsToml::from(&config)).unwrap();
        let decoded: ActiveElectionsToml = toml::from_str(&encoded).unwrap();
        let mut restored = NodeConfig::new_test_instance();
        restored.merge_toml(&crate::config::toml::NodeToml {
            active_elections: Some(decoded),
            ..Default::default()
        });
        assert_eq!(
            restored.active_elections.committee_model,
            config.active_elections.committee_model
        );
        assert!(toml::from_str::<ActiveElectionsToml>("committee_model = 'typo'").is_err());
    }

    #[test]
    fn bounded_weight_config_round_trip_and_default_drift() {
        let mut config = NodeConfig::new_test_instance();
        config.active_elections.committee_model = CommitteeModel::BoundedWeight {
            f: 1,
            p: 1,
            drift: 120,
        };
        let encoded = toml::to_string(&ActiveElectionsToml::from(&config)).unwrap();
        let decoded: ActiveElectionsToml = toml::from_str(&encoded).unwrap();
        assert_eq!(merged(decoded), config.active_elections.committee_model);

        let decoded: ActiveElectionsToml =
            toml::from_str("committee_model = 'bounded_weight'").unwrap();
        assert_eq!(
            merged(decoded),
            CommitteeModel::BoundedWeight {
                f: 1,
                p: 1,
                drift: CommitteeModel::DEFAULT_DRIFT
            }
        );
    }

    #[test]
    fn signing_sync_round_trip_and_default() {
        let mut config = NodeConfig::new_test_instance();
        assert_eq!(config.active_elections.signing_sync, SigningSync::Fsync);
        config.active_elections.signing_sync = SigningSync::Full;
        let encoded = toml::to_string(&ActiveElectionsToml::from(&config)).unwrap();
        let mut restored = NodeConfig::new_test_instance();
        restored.merge_toml(&crate::config::toml::NodeToml {
            active_elections: Some(toml::from_str(&encoded).unwrap()),
            ..Default::default()
        });
        assert_eq!(restored.active_elections.signing_sync, SigningSync::Full);
        assert!(toml::from_str::<ActiveElectionsToml>("signing_sync = 'always'").is_err());
    }

    #[test]
    fn signed_vote_log_round_trip() {
        let mut config = NodeConfig::new_test_instance();
        config.active_elections.signed_vote_log = true;
        let encoded = toml::to_string(&ActiveElectionsToml::from(&config)).unwrap();
        let mut restored = NodeConfig::new_test_instance();
        restored.merge_toml(&crate::config::toml::NodeToml {
            active_elections: Some(toml::from_str(&encoded).unwrap()),
            ..Default::default()
        });
        assert!(restored.active_elections.signed_vote_log);
    }

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
        assert_eq!(toml.epoch_duration_ms, Some(0));
        assert_eq!(toml.close_round_timeout_ms, Some(2000));
    }

    /*
     * Test helpers
     */

    fn merged(toml: ActiveElectionsToml) -> CommitteeModel {
        let mut config = NodeConfig::new_test_instance();
        config.merge_toml(&crate::config::toml::NodeToml {
            active_elections: Some(toml),
            ..Default::default()
        });
        config.active_elections.committee_model
    }
}
