use rsnano_ledger::LedgerConstants;
use rsnano_node::node::Node;
use rsnano_rpc_messages::VersionDto;
use std::sync::Arc;
use toml::to_string_pretty;

pub async fn stop(node: Arc<Node>) -> String {
    let txn = node.store.tx_begin_read();
    let store_version = node.store.version.get(&txn).unwrap();
    let protocol_version = node.network_params.network.protocol_version;
    let node_vendor = "Rsnano version string".to_string();
    let store_vendor = node.store.vendor();
    let network = node.network_params.network.current_network;
    let network_identifier = LedgerConstants::beta().genesis.hash();
    let build_info = "build_info".to_string();

    let version_dto = VersionDto::new(
        1,
        store_version,
        protocol_version,
        node_vendor,
        store_vendor,
        network,
        network_identifier,
        build_info,
    );

    to_string_pretty(&version_dto).unwrap()
}

#[cfg(test)]
mod tests {
    use crate::service::responses::test_helpers::setup_rpc_client_and_server;
    use test_helpers::System;

    #[test]
    fn version() {
        let mut system = System::new();
        let node = system.make_node();

        let (rpc_client, server) = setup_rpc_client_and_server(node.clone(), true);

        node.tokio
            .block_on(async { rpc_client.version().await.unwrap() });

        server.abort();
    }
}
