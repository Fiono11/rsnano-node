use crate::{RpcBool, RpcCommand, RpcU64};
use rsnano_types::{BlockHash, QualifiedRoot};
use serde::{Deserialize, Serialize};

impl RpcCommand {
    pub fn final_state() -> Self {
        Self::FinalState
    }
}

/// Order-independent hash of the final ledger state: per account the settled
/// single notarization certificate if there is one, else the cemented frontier
#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FinalStateResponse {
    pub hash: BlockHash,
    /// No election is still collecting first votes
    pub all_terminated: RpcBool,
    /// No election can still gain a notarization certificate
    pub all_settled: RpcBool,
    pub accounts: RpcU64,
    /// Settled slots whose single notarized block is not cemented yet
    pub single_notarized: RpcU64,
    /// Elections that are not settled yet
    pub pending: RpcU64,
    /// Settled slots with a timeout certificate only
    pub empty: RpcU64,
    /// Settled slots with conflicting notarization certificates; their blocks
    /// are not part of the final state
    pub conflicting: Vec<QualifiedRoot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_final_state_command() {
        assert_eq!(
            serde_json::to_string_pretty(&RpcCommand::final_state()).unwrap(),
            r#"{
  "action": "final_state"
}"#
        );
    }

    #[test]
    fn deserialize_final_state_command() {
        let cmd = RpcCommand::final_state();
        let serialized = serde_json::to_string_pretty(&cmd).unwrap();
        let deserialized: RpcCommand = serde_json::from_str(&serialized).unwrap();
        assert!(matches!(deserialized, RpcCommand::FinalState));
    }
}
