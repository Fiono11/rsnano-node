use crate::{RpcCommand, RpcU64};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

impl RpcCommand {
    pub fn rai_status() -> Self {
        Self::RaiStatus
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaiStatusResponse {
    pub genesis_committee: Vec<String>,
    pub open_epoch: RpcU64,
    pub closing_epoch: Option<RpcU64>,
    pub closing_phase: Option<String>,
    pub closed_through: Option<RpcU64>,
    pub cut_hashes: BTreeMap<String, String>,
    pub close_hashes: BTreeMap<String, String>,
    pub drain_obligations: BTreeMap<String, RpcU64>,
    pub drain_finalized: BTreeMap<String, RpcU64>,
    pub finalized_by_epoch: BTreeMap<String, RpcU64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_command() {
        assert_eq!(
            serde_json::to_string(&RpcCommand::rai_status()).unwrap(),
            r#"{"action":"rai_status"}"#
        );
    }
}
