mod block_processor_config;
mod bootstrap_ascending_config;
mod bootstrap_server_config;
mod diagnostics_config;
mod hinted_scheduler_config;
mod monitor_config;
mod node_config;
mod node_rpc_config;
mod opencl_config;
mod optimistic_scheduler_config;
mod stats_config;
mod vote_cache;
mod websocket_config;

pub use block_processor_config::*;
pub use bootstrap_ascending_config::*;
pub use bootstrap_server_config::*;
pub use diagnostics_config::*;
pub use hinted_scheduler_config::*;
pub use monitor_config::*;
pub use node_config::*;
pub use node_rpc_config::*;
pub use opencl_config::*;
pub use optimistic_scheduler_config::*;
pub use stats_config::*;
pub use vote_cache::*;
pub use websocket_config::*;

use serde::{de::Error, Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone)]
pub struct Miliseconds(pub(crate) u128);

impl Serialize for Miliseconds {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Miliseconds {
    fn deserialize<D>(deserializer: D) -> Result<Miliseconds, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let miliseconds = s.parse::<u128>().map_err(Error::custom)?;
        Ok(Miliseconds(miliseconds))
    }
}
