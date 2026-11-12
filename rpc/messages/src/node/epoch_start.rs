use serde::{Deserialize, Serialize};

use crate::{RpcCommand, RpcU64};

impl RpcCommand {
    /// RAI: the setup is over, the consensus epochs start now
    pub fn epoch_start() -> Self {
        Self::EpochStart(EpochStartArgs::default())
    }

    /// RAI: the consensus epochs start at a common wall-clock instant, in
    /// unix milliseconds, so that every node shares the epoch boundaries
    /// whenever the request reaches it
    pub fn epoch_start_at(origin_unix_ms: u64) -> Self {
        Self::EpochStart(EpochStartArgs {
            origin_unix_ms: Some(origin_unix_ms.into()),
        })
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EpochStartArgs {
    /// RAI: the epochs' origin in unix milliseconds; without it, the moment
    /// the request arrives
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_unix_ms: Option<RpcU64>,
}

/// RAI: the epoch the node is in
#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EpochStartResponse {
    pub epoch: RpcU64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialize_epoch_start() {
        let command = RpcCommand::epoch_start();
        let serialized = serde_json::to_value(command).unwrap();
        assert_eq!(serialized, json!({"action": "epoch_start"}));
    }

    #[test]
    fn deserialize_epoch_start() {
        let json = json!({"action": "epoch_start"});
        let deserialized: RpcCommand = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized, RpcCommand::epoch_start());
    }

    #[test]
    fn epoch_start_with_a_common_origin_round_trips() {
        let command = RpcCommand::epoch_start_at(1_794_400_000_123);
        let serialized = serde_json::to_value(&command).unwrap();
        assert_eq!(
            serialized,
            json!({"action": "epoch_start", "origin_unix_ms": "1794400000123"})
        );
        let deserialized: RpcCommand = serde_json::from_value(serialized).unwrap();
        assert_eq!(deserialized, command);
    }
}
