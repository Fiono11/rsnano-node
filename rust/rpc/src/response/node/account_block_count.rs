use rsnano_core::Account;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::{json, to_string_pretty};
use std::sync::Arc;

#[derive(Serialize)]
struct AccountBlockCount {
    block_count: String,
}

impl AccountBlockCount {
    fn new(block_count: String) -> Self {
        Self { block_count }
    }
}

pub(crate) async fn account_block_count(node: Arc<Node>, account_str: String) -> String {
    let tx = node.ledger.read_txn();
    match Account::decode_account(&account_str) {
        Ok(account) => match node.ledger.store.account.get(&tx, &account) {
            Some(account_info) => {
                let account_block_count =
                    AccountBlockCount::new(account_info.block_count.to_string());
                to_string_pretty(&account_block_count).unwrap()
            }
            None => to_string_pretty(&json!({ "error": "Account not found" })).unwrap(),
        },
        Err(_) => to_string_pretty(&json!({ "error": "Bad account number" })).unwrap(),
    }
}
