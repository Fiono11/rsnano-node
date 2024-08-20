use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::{sync::Arc, time::Instant};

#[derive(Serialize)]
struct Uptime {
    seconds: String,
}

impl Uptime {
    fn new(seconds: String) -> Self {
        Self { seconds }
    }
}

pub(crate) async fn uptime(node: Arc<Node>) -> String {
    let seconds = Instant::now() - node.telemetry.startup_time;
    let uptime = Uptime::new(seconds.as_secs().to_string());
    to_string_pretty(&uptime).unwrap()
}
