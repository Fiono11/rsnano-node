use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountRepresentative {
    representative: String,
}

impl AccountRepresentative {
    fn new(representative: String) -> Self {
        Self { representative }
    }
}

impl Service {
    pub(crate) async fn account_representative(&self, account_str: String) -> String {
        let tx = self.node.ledger.read_txn();
        match Account::decode_account(&account_str) {
            Ok(account) => match self.node.ledger.store.account.get(&tx, &account) {
                Some(account_info) => {
                    let account_representative =
                        AccountRepresentative::new(account_info.representative.encode_account());
                    to_string_pretty(&account_representative).unwrap()
                }
                None => {
                    let error = json!({ "error": "Account not found" });
                    to_string_pretty(&error).unwrap()
                }
            },
            Err(_) => {
                let error = json!({ "error": "Bad account number" });
                to_string_pretty(&error).unwrap()
            }
        }
    }
}

pub(crate) async fn handle_account_representative(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    if let Some(account) = rpc_request.account {
        Ok(service.account_representative(account).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
