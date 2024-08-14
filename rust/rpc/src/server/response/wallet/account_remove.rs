use crate::format_error_message;
use rsnano_core::{Account, WalletId};
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct AccountRemove {
    removed: String,
}

impl AccountRemove {
    fn new(removed: String) -> Self {
        Self { removed }
    }
}

pub(crate) async fn account_remove(node: Arc<Node>, wallet: String, account: String) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet_id) => match Account::decode_account(&account) {
            Ok(account) => match node.wallets.remove_account(&wallet_id, &account) {
                Ok(_) => to_string_pretty(&AccountRemove::new("1".to_string())).unwrap(),
                Err(_) => format_error_message("Failed to remove account"),
            },
            Err(_) => format_error_message("Invalid account"),
        },
        Err(_) => format_error_message("Bad wallet"),
    }
}
