use serde::{Deserialize, Serialize};

use crate::{RpcCommand, RpcU64};

impl RpcCommand {
    /// RAI: leave the current consensus epoch now
    pub fn epoch_advance() -> Self {
        Self::EpochAdvance
    }
}

/// RAI: the epoch the node is in after leaving the previous one
#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EpochAdvanceResponse {
    pub epoch: RpcU64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialize_epoch_advance() {
        let command = RpcCommand::epoch_advance();
        let serialized = serde_json::to_value(command).unwrap();
        assert_eq!(serialized, json!({"action": "epoch_advance"}));
    }

    #[test]
    fn deserialize_epoch_advance() {
        let json = json!({"action": "epoch_advance"});
        let deserialized: RpcCommand = serde_json::from_value(json).unwrap();
        assert!(matches!(deserialized, RpcCommand::EpochAdvance));
    }
}
