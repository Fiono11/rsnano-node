use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountHistory {
    account: String,
    history: History,
    previous: String,
}

impl AccountHistory {
    fn new(account: String, history: History, previous: String) -> Self {
        Self {
            account,
            history,
            previous,
        }
    }
}

#[derive(Serialize)]
struct History {
    r#type: String,
    account: String,
    amount: String,
    local_timestamp: String,
    height: String,
    hash: String,
    confirmed: String,
}

impl History {
    fn new(
        r#type: String,
        account: String,
        amount: String,
        local_timestamp: String,
        height: String,
        hash: String,
        confirmed: String,
    ) -> Self {
        Self {
            r#type,
            account,
            amount,
            local_timestamp,
            height,
            hash,
            confirmed,
        }
    }
}

impl Service {
    pub(crate) async fn account_history(&self, account_str: String) -> String {
        todo!()
    }
}

pub(crate) async fn handle_account_history(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    if let Some(account) = rpc_request.account {
        Ok(service.account_history(account).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
