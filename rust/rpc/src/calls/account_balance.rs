use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountBalance {
    balance: String,
    pending: String,
    receivable: String,
}

impl AccountBalance {
    fn new(balance: String, pending: String, receivable: String) -> Self {
        Self {
            balance,
            pending,
            receivable,
        }
    }
}

impl Service {
    pub(crate) async fn account_balance(
        &self,
        account_str: String,
        only_confirmed: bool,
    ) -> String {
        let tx = self.node.ledger.read_txn();
        match Account::decode_account(&account_str) {
            Ok(account) => {
                let balance = match self.node.ledger.confirmed().account_balance(&tx, &account) {
                    Some(balance) => balance,
                    None => return "Account not found".to_string(),
                };
                let pending = self
                    .node
                    .ledger
                    .account_receivable(&tx, &account, only_confirmed);
                let account = AccountBalance::new(
                    balance.number().to_string(),
                    pending.number().to_string(),
                    pending.number().to_string(),
                );
                to_string_pretty(&account).unwrap()
            }
            Err(_) => to_string_pretty(&json!({ "error": "Bad account number" })).unwrap(),
        }
    }
}

pub(crate) async fn handle_account_balance(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    let only_confirmed = rpc_request.only_confirmed.unwrap_or(true);
    if let Some(account) = rpc_request.account {
        Ok(service.account_balance(account, only_confirmed).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
