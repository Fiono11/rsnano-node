use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountBlockCount {
    block_count: String,
}

impl Service {
    pub(crate) async fn account_block_count(&self, account_str: String) -> String {
        let tx = self.node.ledger.read_txn();
        match Account::decode_account(&account_str) {
            Ok(account) => match self.node.ledger.store.account.get(&tx, &account) {
                Some(account_info) => {
                    let account_block_count = AccountBlockCount {
                        block_count: account_info.block_count.to_string(),
                    };
                    to_string_pretty(&account_block_count).unwrap()
                }
                None => to_string_pretty(&json!({ "error": "Account not found" })).unwrap(),
            },
            Err(_) => to_string_pretty(&json!({ "error": "Bad account number" })).unwrap(),
        }
    }
}

pub(crate) async fn handle_account_block_count(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    if let Some(account) = rpc_request.account {
        Ok(service.account_block_count(account).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
