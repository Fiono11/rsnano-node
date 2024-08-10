use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountKey {
    key: String,
}

impl AccountKey {
    fn new(key: String) -> Self {
        Self { key }
    }
}

impl Service {
    pub(crate) async fn account_key(&self, account: String) -> String {
        match Account::decode_account(&account) {
            Ok(account) => to_string_pretty(&AccountKey::new(account.encode_hex())).unwrap(),
            Err(_) => to_string_pretty(&json!({ "error": "Bad account number" })).unwrap(),
        }
    }
}

pub(crate) async fn handle_account_key(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    if let Some(account) = rpc_request.account {
        Ok(service.account_key(account).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
