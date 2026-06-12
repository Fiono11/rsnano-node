use std::time::Duration;

use rsnano_ledger::DEV_GENESIS_PUB_KEY;
use rsnano_types::{
    Account, Amount, NetworkType, PublicKey,
    currency_constants::PRECONFIGURED_REPRESENTATIVES_BETA, default_preconfigured_representatives,
};

#[derive(Clone)]
pub struct WalletsConfig {
    pub preconfigured_representatives: Vec<PublicKey>,
    pub password_fanout: usize,
    pub receive_minimum: Amount,
    pub vote_minimum: Amount,
    pub voting_enabled: bool,
    /// How long to wait until the next cached work is created
    pub cached_work_generation_delay: Duration,
    pub kdf_work: u32,
}

impl WalletsConfig {
    pub fn default_for(network: NetworkType) -> Self {
        match network {
            NetworkType::Invalid => unreachable!(),
            NetworkType::NanoDevNetwork => Self::defaults_dev(),
            NetworkType::NanoBetaNetwork => Self::defaults_beta(),
            NetworkType::NanoLiveNetwork => Self::defaults_live(),
            NetworkType::NanoTestNetwork => Self::defaults_test(),
        }
    }

    pub fn defaults_live() -> Self {
        Self {
            preconfigured_representatives: default_preconfigured_representatives(
                NetworkType::NanoLiveNetwork,
            ),
            password_fanout: 1024,
            receive_minimum: Amount::micronano(1),
            vote_minimum: Amount::nano(1000),
            voting_enabled: false,
            cached_work_generation_delay: Duration::from_secs(10),
            kdf_work: 1024 * 64,
        }
    }

    pub fn defaults_dev() -> Self {
        Self {
            voting_enabled: true,
            preconfigured_representatives: vec![*DEV_GENESIS_PUB_KEY],
            cached_work_generation_delay: Duration::from_secs(1),
            kdf_work: 8,
            ..Self::defaults_live()
        }
    }

    pub fn defaults_beta() -> Self {
        Self {
            preconfigured_representatives: PRECONFIGURED_REPRESENTATIVES_BETA
                .iter()
                .map(|r| Account::parse(r).unwrap().into())
                .collect(),
            ..Self::defaults_live()
        }
    }

    pub fn defaults_test() -> Self {
        Self {
            preconfigured_representatives: Vec::new(),
            ..Self::defaults_live()
        }
    }
}

impl Default for WalletsConfig {
    fn default() -> Self {
        Self::defaults_live()
    }
}
