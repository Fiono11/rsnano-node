use rsnano_core::Account;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::{json, to_string_pretty};
use std::sync::Arc;

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
pub(crate) async fn account_balance(
    node: Arc<Node>,
    account_str: String,
    only_confirmed: Option<bool>,
) -> String {
    let tx = node.ledger.read_txn();
    match Account::decode_account(&account_str) {
        Ok(account) => match node.ledger.confirmed().account_balance(&tx, &account) {
            Some(balance) => {
                let pending =
                    node.ledger
                        .account_receivable(&tx, &account, only_confirmed.unwrap_or(true));
                let account = AccountBalance::new(
                    balance.number().to_string(),
                    pending.number().to_string(),
                    pending.number().to_string(),
                );
                to_string_pretty(&account).unwrap()
            }
            None => to_string_pretty(&json!({ "error": "Account not found" })).unwrap(),
        },
        Err(_) => to_string_pretty(&json!({ "error": "Bad account number" })).unwrap(),
    }
}
