use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::BlockHash;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct BlockAccount {
    account: String,
}

impl BlockAccount {
    fn new(account: String) -> Self {
        Self { account }
    }
}

impl Service {
    pub(crate) async fn block_account(&self, hash_str: String) -> String {
        let tx = self.node.ledger.read_txn();
        match BlockHash::decode_hex(&hash_str) {
            Ok(hash) => match &self.node.ledger.any().get_block(&tx, &hash) {
                Some(block) => {
                    let account = block.account();
                    let block_account = BlockAccount::new(account.encode_account());
                    to_string_pretty(&block_account).unwrap()
                }
                None => to_string_pretty(&json!({ "error": "Block not found" })).unwrap(),
            },
            Err(_) => to_string_pretty(&json!({ "error": "Account not found" })).unwrap(),
        }
    }
}

pub(crate) async fn handle_block_account(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    if let Some(hash) = rpc_request.hash {
        Ok(service.block_account(hash).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
