use serde::{Deserialize, Serialize};

use crate::{RpcCommand, RpcU64};

impl RpcCommand {
    /// RAI: the setup is over, the consensus epochs start now
    pub fn epoch_start() -> Self {
        Self::EpochStart
    }
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
        assert!(matches!(deserialized, RpcCommand::EpochStart));
    }
}
