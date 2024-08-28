use rsnano_core::WalletId;
use rsnano_node::{node::Node, wallets::WalletsExt};
use rsnano_rpc_messages::WalletCreatedDto;
use serde_json::to_string_pretty;
use std::sync::Arc;

pub async fn wallet_create(node: Arc<Node>) -> String {
    let wallet = WalletId::random();

    node.wallets.create(wallet);

    to_string_pretty(&WalletCreatedDto::new(wallet)).unwrap()
}

#[cfg(test)]
mod tests {
    use crate::service::responses::test_helpers::setup_rpc_client_and_server;
    use test_helpers::System;

    #[test]
    fn wallet_create() {
        let mut system = System::new();
        let node = system.make_node();

        let (rpc_client, server) = setup_rpc_client_and_server(node.clone());

        let result = node
            .tokio
            .block_on(async { rpc_client.wallet_create().await.unwrap() });

        let wallets = node.wallets.wallet_ids();

        assert!(wallets.contains(&result.wallet));

        server.abort();
    }
}