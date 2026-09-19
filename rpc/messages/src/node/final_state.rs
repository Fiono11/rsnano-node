use crate::{RpcBool, RpcCommand, RpcU64};
use rsnano_types::{Account, BlockHash, QualifiedRoot};
use serde::{Deserialize, Serialize};

impl RpcCommand {
    pub fn final_state() -> Self {
        Self::FinalState(FinalStateArgs { epoch: None })
    }

    /// With the entries of one epoch listed
    pub fn final_state_entries(epoch: u64) -> Self {
        Self::FinalState(FinalStateArgs {
            epoch: Some(epoch.into()),
        })
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct FinalStateArgs {
    /// RAI: list the entries of this epoch's state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epoch: Option<RpcU64>,
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
    pub conflicting: Vec<ConflictingRoot>,
    /// RAI: the consensus epoch new elections are started in
    pub current_epoch: RpcU64,
    /// RAI: the final state of every consensus epoch this node took part in
    pub epochs: Vec<EpochFinalState>,
    /// RAI: the entries of the requested epoch's state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<FinalStateEntry>>,
}

/// RAI: one entry of an epoch's final state
#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FinalStateEntry {
    pub account: Account,
    pub height: RpcU64,
    pub hash: BlockHash,
    /// "finalized" or "single"
    pub kind: String,
}

/// RAI: what one consensus epoch decided on this node
#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EpochFinalState {
    pub epoch: RpcU64,
    /// Order-independent hash of the blocks finalized by a certificate of
    /// this epoch and of the settled single notarization certificates
    pub hash: BlockHash,
    pub finalized: RpcU64,
    pub single_notarized: RpcU64,
    /// Elections of this epoch that are not settled and whose block is not cemented
    pub pending: RpcU64,
    /// Elections of this epoch that are not settled although their block is
    /// cemented; they are still collecting the certificates of this epoch
    pub cemented_undecided: RpcU64,
    pub empty: RpcU64,
    pub conflicting: RpcU64,
    /// The close election of the epoch, once this node has left the epoch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close: Option<EpochCloseState>,
}

/// RAI: the close election of one epoch as seen by this node
#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EpochCloseState {
    /// Every instance of the epoch settled here: the node attests `value`
    pub ready: RpcBool,
    /// The value this node attests, derived from `hash`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<BlockHash>,
    /// This node takes part: it is ready and the epoch before is closed
    pub started: RpcBool,
    /// The round this node is in
    pub round: RpcU64,
    /// The value a certificate of the close election finalized
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_value: Option<BlockHash>,
    /// The round that finalized it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_round: Option<RpcU64>,
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConflictingRoot {
    pub root: QualifiedRoot,
    /// RAI consensus epoch of the election
    pub epoch: RpcU64,
    /// Every candidate of the election, i.e. the blocks that are discarded
    pub blocks: Vec<BlockHash>,
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
        assert_eq!(
            serde_json::to_string_pretty(&RpcCommand::final_state_entries(1)).unwrap(),
            r#"{
  "action": "final_state",
  "epoch": "1"
}"#
        );
    }

    #[test]
    fn deserialize_final_state_command() {
        let cmd = RpcCommand::final_state();
        let serialized = serde_json::to_string_pretty(&cmd).unwrap();
        let deserialized: RpcCommand = serde_json::from_str(&serialized).unwrap();
        assert!(matches!(deserialized, RpcCommand::FinalState(_)));
    }
}
