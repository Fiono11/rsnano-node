use rsnano_node::{Node, NodeExt};
use rsnano_rpc_messages::{ErrorDto, SuccessDto};
use serde_json::to_string_pretty;
use std::sync::Arc;

pub async fn search_receivable_all(node: Arc<Node>, enable_control: bool) -> String {
    if enable_control {
        node.search_receivable_all();
        to_string_pretty(&SuccessDto::new()).unwrap()
    } else {
        to_string_pretty(&ErrorDto::new("RPC control is disabled".to_string())).unwrap()
    }
}
