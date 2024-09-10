use rsnano_node::{node::Node, stats::StatCategory};
use rsnano_rpc_messages::StatsDto;
use serde_json::to_string_pretty;
use std::{sync::Arc, time::{SystemTime, UNIX_EPOCH}};

pub async fn stats(node: Arc<Node>, stat_category: StatCategory) -> String {
    let stats = node.stats.mutables.read().unwrap();
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

    match stat_category {
        StatCategory::Counters => {
            to_string_pretty(&StatsDto::new(StatCategory::Counters, &stats.counters.keys().collect::<Vec<_>>(), timestamp)).unwrap()
        }
        StatCategory::Samples => {
            to_string_pretty(&StatsDto::new(StatCategory::Samples, &stats.samplers.keys().collect::<Vec<_>>(), timestamp)).unwrap()
        }
    }
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