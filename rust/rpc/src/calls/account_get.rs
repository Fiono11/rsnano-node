use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountGet {
    account: String,
}

impl AccountGet {
    fn new(account: String) -> Self {
        Self { account }
    }
}

impl Service {
    pub(crate) async fn account_get(&self, key: String) -> String {
        match Account::decode_hex(&key) {
            Ok(pk) => to_string_pretty(&AccountGet::new(pk.encode_account())).unwrap(),
            Err(_) => to_string_pretty(&json!({ "error": "Bad public key" })).unwrap(),
        }
    }
}

pub(crate) async fn handle_account_get(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    if let Some(key) = rpc_request.key {
        Ok(service.account_get(key).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
