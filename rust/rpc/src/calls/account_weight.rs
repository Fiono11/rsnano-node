use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountWeight {
    weight: String,
}

impl AccountWeight {
    fn new(weight: String) -> Self {
        Self { weight }
    }
}

impl Service {
    pub(crate) async fn account_weight(&self, account_str: String) -> String {
        let tx = self.node.ledger.read_txn();
        match Account::decode_account(&account_str) {
            Ok(account) => {
                let weight = self.node.ledger.weight_exact(&tx, account).to_string_dec();
                let account_weight = AccountWeight::new(weight);
                to_string_pretty(&account_weight).unwrap()
            }
            Err(_) => {
                let error = json!({ "error": "Bad account number" });
                to_string_pretty(&error).unwrap()
            }
        }
    }
}

pub(crate) async fn handle_account_weight(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    if let Some(account) = rpc_request.account {
        Ok(service.account_weight(account).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
