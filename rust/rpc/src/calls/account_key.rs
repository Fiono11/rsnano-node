use crate::server::{RpcRequest, Service};
use anyhow::{anyhow, Result};
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountKey {
    key: String,
}

impl Service {
    pub(crate) async fn account_key(&self, account: String) -> String {
        match Account::decode_account(&account) {
            Ok(account) => {
                let account_key = AccountKey {
                    key: account.encode_hex(),
                };
                to_string_pretty(&account_key).unwrap()
            }
            Err(_) => {
                let error = json!({ "error": "Bad account number" });
                to_string_pretty(&error).unwrap()
            }
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
        Err(anyhow!(to_string_pretty(
            &json!({ "error": "Unable to parse JSON" })
        )
        .unwrap()))
    }
}
