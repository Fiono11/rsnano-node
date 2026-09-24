use serde::{Deserialize, Serialize};

use rsnano_types::{Account, BlockHash};

use crate::{RpcCommand, RpcU64};

impl RpcCommand {
    /// RAI: the locks of the latest checkpoint decided on the node
    pub fn epoch_locks() -> Self {
        Self::EpochLocks
    }
}

/// RAI: the positions the latest decided checkpoint keeps as locks, which an
/// owner resolves with a fresh child of one of the locked blocks
#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EpochLocksResponse {
    /// The epoch of the checkpoint, none before the first one is decided
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epoch: Option<RpcU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_hash: Option<BlockHash>,
    pub locks: Vec<EpochLock>,
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EpochLock {
    pub account: Account,
    pub height: RpcU64,
    pub hash: BlockHash,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialize_epoch_locks() {
        let command = RpcCommand::epoch_locks();
        let serialized = serde_json::to_value(command).unwrap();
        assert_eq!(serialized, json!({"action": "epoch_locks"}));
    }

    #[test]
    fn deserialize_epoch_locks() {
        let json = json!({"action": "epoch_locks"});
        let deserialized: RpcCommand = serde_json::from_value(json).unwrap();
        assert!(matches!(deserialized, RpcCommand::EpochLocks));
    }
}
