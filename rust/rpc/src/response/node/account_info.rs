use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountInfo {
    frontier: String,
    open_block: String,
    representative_block: String,
    balance: String,
    modified_timestamp: String,
    block_count: String,
    account_version: String,
    confirmation_height: String,
    confirmation_height_frontier: String,
}

impl AccountInfo {
    fn new(
        frontier: String,
        open_block: String,
        representative_block: String,
        balance: String,
        modified_timestamp: String,
        block_count: String,
        account_version: String,
        confirmation_height: String,
        confirmation_height_frontier: String,
    ) -> Self {
        Self {
            frontier,
            open_block,
            representative_block,
            balance,
            modified_timestamp,
            block_count,
            account_version,
            confirmation_height,
            confirmation_height_frontier,
        }
    }
}

impl Service {
    pub(crate) async fn account_info(&self, account_str: String) -> String {
        todo!()
    }
}

pub(crate) async fn handle_account_info(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    if let Some(account) = rpc_request.account {
        Ok(service.account_info(account).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
