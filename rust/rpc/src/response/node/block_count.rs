use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct BlockCount {
    count: String,
    unchecked: String,
    cemented: Option<String>,
}

impl BlockCount {
    fn new(count: String, unchecked: String, cemented: Option<String>) -> Self {
        Self {
            count,
            unchecked,
            cemented,
        }
    }
}

pub(crate) async fn block_count(node: Arc<Node>, include_cemented: Option<bool>) -> String {
    let include_cemented = include_cemented.unwrap_or(true);
    let count = node.ledger.block_count().to_string();
    let unchecked = node.unchecked.buffer_count().to_string();
    if include_cemented {
        let cemented = node.ledger.cemented_count().to_string();
        let block_count = BlockCount::new(count, unchecked, Some(cemented));
        to_string_pretty(&block_count).unwrap()
    } else {
        let block_count = BlockCount::new(count, unchecked, None);
        to_string_pretty(&block_count).unwrap()
    }
}
