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
    pub open_epoch: RpcU64,
    pub closing_epoch: Option<RpcU64>,
    pub closed_through: Option<RpcU64>,
    pub close_hashes: BTreeMap<String, String>,
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
