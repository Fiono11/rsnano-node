use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::BlockHash;
use serde::Serialize;
use serde_json::to_string_pretty;

#[derive(Serialize)]
struct BlockCount {
    count: String,
    unchecked: String,
    cemented: String,
}

impl BlockCount {
    fn new(count: String, unchecked: String, cemented: String) -> Self {
        Self {
            count,
            unchecked,
            cemented,
        }
    }
}

impl Service {
    pub(crate) async fn block_count(&self) -> String {
        let count = self.node.ledger.block_count().to_string();
        let unchecked = self.node.unchecked.buffer_count().to_string();
        let cemented = self.node.ledger.cemented_count().to_string();
        let block_count = BlockCount::new(count, unchecked, cemented);
        to_string_pretty(&block_count).unwrap()
    }
}
