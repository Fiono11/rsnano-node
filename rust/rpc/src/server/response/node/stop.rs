use rsnano_node::node::{Node, NodeExt};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct Stop {
    success: String,
}

impl Stop {
    fn new(success: String) -> Self {
        Self { success }
    }
}

pub(crate) async fn stop(node: Arc<Node>) -> String {
    node.stop();
    let stop = Stop::new(String::new());
    to_string_pretty(&stop).unwrap()
}
