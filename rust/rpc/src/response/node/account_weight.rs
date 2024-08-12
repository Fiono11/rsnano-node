use rsnano_core::Account;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::{json, to_string_pretty};
use std::sync::Arc;

#[derive(Serialize)]
struct AccountWeight {
    weight: String,
}

impl AccountWeight {
    fn new(weight: String) -> Self {
        Self { weight }
    }
}

pub(crate) async fn account_weight(node: Arc<Node>, account_str: String) -> String {
    let tx = node.ledger.read_txn();
    match Account::decode_account(&account_str) {
        Ok(account) => {
            let weight = node.ledger.weight_exact(&tx, account).to_string_dec();
            let account_weight = AccountWeight::new(weight);
            to_string_pretty(&account_weight).unwrap()
        }
        Err(_) => to_string_pretty(&json!({ "error": "Bad account number" })).unwrap(),
    }
}
