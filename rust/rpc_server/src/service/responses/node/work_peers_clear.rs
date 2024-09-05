use std::sync::Arc;
use rsnano_node::node::Node;
use rsnano_rpc_messages::SuccessDto;
use serde_json::to_string_pretty;

pub async fn work_peers_clear(node: Arc<Node>) -> String {
    node.config.work_peers.clear();
    to_string_pretty(&SuccessDto::new()).unwrap()
}

#[cfg(test)]
mod tests {
    use crate::service::responses::test_helpers::setup_rpc_client_and_server;
    use test_helpers::System;

    #[test]
    fn work_peers_clear() {
        let mut system = System::new();
        let node = system.make_node();

        let (rpc_client, server) = setup_rpc_client_and_server(node.clone(), true);

        node.tokio.block_on(async {
            rpc_client
                .work_peers_clear()
                .await
                .unwrap()
        });

        assert!(node.config.work_peers.is_empty());
    
        server.abort();
    }
}