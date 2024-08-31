use rsnano_core::PublicKey;
use rsnano_rpc_messages::AccountRpcMessage;
use serde_json::to_string_pretty;

pub async fn account_get(public_key: PublicKey) -> String {
    to_string_pretty(&AccountRpcMessage::new(
        "account".to_string(),
        public_key.as_account(),
    ))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use crate::service::responses::test_helpers::setup_rpc_client_and_server;
    use rsnano_core::{PublicKey, WalletId};
    use rsnano_node::wallets::WalletsExt;
    use test_helpers::System;

    #[test]
    fn account_get() {
        let mut system = System::new();
        let node = system.make_node();

        let (rpc_client, server) = setup_rpc_client_and_server(node.clone(), true);

        let wallet_id = WalletId::random();

        node.wallets.create(wallet_id);

        let public_key = PublicKey::decode_hex(
            "3068BB1CA04525BB0E416C485FE6A67FD52540227D267CC8B6E8DA958A7FA039",
        )
        .unwrap();

        let result = node
            .tokio
            .block_on(async { rpc_client.account_get(public_key).await.unwrap() });

        assert_eq!(result.value, public_key.as_account());

        server.abort();
    }
}