use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

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

pub(crate) async fn block_count(node: Arc<Node>) -> String {
    let count = node.ledger.block_count().to_string();
    let unchecked = node.unchecked.buffer_count().to_string();
    let cemented = node.ledger.cemented_count().to_string();
    let block_count = BlockCount::new(count, unchecked, cemented);
    to_string_pretty(&block_count).unwrap()
}
