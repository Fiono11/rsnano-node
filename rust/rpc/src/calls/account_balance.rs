use crate::server::Service;
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
    pub async fn account_balance(&self, account_str: String, only_confirmed: bool) -> String {
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
            Err(_) => {
                let error = json!({ "error": "Bad account number" });
                to_string_pretty(&error).unwrap()
            }
        }
    }
}
