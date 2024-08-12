use rsnano_core::BlockHash;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::{json, to_string_pretty};
use std::sync::Arc;

#[derive(Serialize)]
struct BlockAccount {
    account: String,
}

impl BlockAccount {
    fn new(account: String) -> Self {
        Self { account }
    }
}

pub(crate) async fn block_account(node: Arc<Node>, hash_str: String) -> String {
    let tx = node.ledger.read_txn();
    match BlockHash::decode_hex(&hash_str) {
        Ok(hash) => match &node.ledger.any().get_block(&tx, &hash) {
            Some(block) => {
                let account = block.account();
                let block_account = BlockAccount::new(account.encode_account());
                to_string_pretty(&block_account).unwrap()
            }
            None => to_string_pretty(&json!({ "error": "Block not found" })).unwrap(),
        },
        Err(_) => to_string_pretty(&json!({ "error": "Invalid block hash" })).unwrap(),
    }
}
