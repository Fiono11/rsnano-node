use crate::server::{RpcRequest, Service};
use anyhow::{anyhow, Result};
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountGet {
    account: String,
}

impl Service {
    pub(crate) async fn account_get(&self, key: String) -> String {
        match Account::decode_hex(&key) {
            Ok(pk) => {
                let account_get = AccountGet {
                    account: pk.encode_account(),
                };
                to_string_pretty(&account_get).unwrap()
            }
            Err(_) => {
                let error = json!({ "error": "Bad public key" });
                to_string_pretty(&error).unwrap()
            }
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
        Err(anyhow!(to_string_pretty(
            &json!({ "error": "Unable to parse JSON" })
        )
        .unwrap()))
    }
}
