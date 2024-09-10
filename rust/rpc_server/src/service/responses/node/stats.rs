use rsnano_node::{node::Node, stats::StatCategory};
use serde_json::{json, Value};
use std::{sync::Arc, time::{SystemTime, UNIX_EPOCH}};

pub async fn stats(node: Arc<Node>, stat_category: StatCategory) -> String {
    let stats = node.stats.mutables.read().unwrap();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    //let created = chrono::Utc::now().format("%Y.%m.%d %H:%M:%S").to_string();

    let entries: Value = match stat_category {
        StatCategory::Counters => json!(stats.counters.keys().collect::<Vec<_>>()),
        StatCategory::Samples => json!(stats.samplers),
    };

    let result = json!({
        "type": match stat_category {
            StatCategory::Counters => "counters",
            StatCategory::Samples => "samples",
        },
        "created": timestamp,
        "entries": entries,
    });

    serde_json::to_string_pretty(&result).unwrap()
}

#[cfg(test)]
mod tests {
    use crate::service::responses::test_helpers::setup_rpc_client_and_server;
    use test_helpers::System;

    #[test]
    fn stats() {
        let mut system = System::new();
        let node = system.make_node();

        let (rpc_client, server) = setup_rpc_client_and_server(node.clone(), false);

        let result = node.tokio.block_on(async {
            rpc_client
                .stats(rsnano_node::stats::StatCategory::Counters)
                .await
                .unwrap()
        });

        server.abort();
    }
}