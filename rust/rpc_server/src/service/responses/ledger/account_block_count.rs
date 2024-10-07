use rsnano_core::Account;
use rsnano_node::node::Node;
use rsnano_rpc_messages::{ErrorDto, U64RpcMessage};
use serde_json::to_string_pretty;
use std::sync::Arc;

pub async fn account_block_count(node: Arc<Node>, account: Account) -> String {
    let tx = node.ledger.read_txn();
    match node.ledger.store.account.get(&tx, &account) {
        Some(account_info) => {
            let account_block_count =
                U64RpcMessage::new("block_count".to_string(), account_info.block_count);
            to_string_pretty(&account_block_count).unwrap()
        }
        None => to_string_pretty(&ErrorDto::new("Account not found".to_string())).unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use rsnano_core::Account;
    use rsnano_ledger::DEV_GENESIS_ACCOUNT;
    use test_helpers::{setup_rpc_client_and_server, System};

    #[test]
    fn account_block_count() {
        let mut system = System::new();
        let node = system.make_node();

        let (rpc_client, server) = setup_rpc_client_and_server(node.clone(), true);

        let result = node.tokio.block_on(async {
            rpc_client
                .account_block_count(DEV_GENESIS_ACCOUNT.to_owned())
                .await
                .unwrap()
        });

        assert_eq!(result.value, 1);

        server.abort();
    }

    #[test]
    fn account_block_count_fails_with_account_not_found() {
        let mut system = System::new();
        let node = system.make_node();

        let (rpc_client, server) = setup_rpc_client_and_server(node.clone(), true);

        let result = node
            .tokio
            .block_on(async { rpc_client.account_block_count(Account::zero()).await });

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("node returned error: \"Account not found\"".to_string())
        );

        server.abort();
    }
}
