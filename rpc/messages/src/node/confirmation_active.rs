use crate::{RpcCommand, RpcU64};
use rsnano_types::QualifiedRoot;
use serde::{Deserialize, Serialize};

impl RpcCommand {
    pub fn confirmation_active(announcements: Option<u64>) -> Self {
        Self::ConfirmationActive(ConfirmationActiveArgs {
            termination_audit_offset: None,
            block_tree: None,
            announcements: announcements.map(|i| i.into()),
        })
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConfirmationActiveArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_tree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_audit_offset: Option<RpcU64>,
    pub announcements: Option<RpcU64>,
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConfirmationActiveResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_tree: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_audit: Option<serde_json::Value>,
    pub confirmations: Vec<QualifiedRoot>,
    pub unconfirmed: RpcU64,
    pub confirmed: RpcU64,
}
