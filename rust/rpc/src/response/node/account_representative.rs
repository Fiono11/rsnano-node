use rsnano_core::Account;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::{json, to_string_pretty};
use std::sync::Arc;

#[derive(Serialize)]
struct AccountRepresentative {
    representative: String,
}

impl AccountRepresentative {
    fn new(representative: String) -> Self {
        Self { representative }
    }
}

pub(crate) async fn account_representative(node: Arc<Node>, account_str: String) -> String {
    let tx = node.ledger.read_txn();
    match Account::decode_account(&account_str) {
        Ok(account) => match node.ledger.store.account.get(&tx, &account) {
            Some(account_info) => {
                let account_representative =
                    AccountRepresentative::new(account_info.representative.encode_account());
                to_string_pretty(&account_representative).unwrap()
            }
            None => to_string_pretty(&json!({ "error": "Account not found" })).unwrap(),
        },
        Err(_) => to_string_pretty(&json!({ "error": "Bad account number" })).unwrap(),
    }
}
