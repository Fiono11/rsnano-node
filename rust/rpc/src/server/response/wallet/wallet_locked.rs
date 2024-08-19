use crate::format_error_message;
use rsnano_core::WalletId;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct WalletLocked {
    locked: String,
}

impl WalletLocked {
    fn new(locked: String) -> Self {
        Self { locked }
    }
}

pub(crate) async fn wallet_locked(node: Arc<Node>, wallet: String) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet_id) => {
            let mut wallet_locked = WalletLocked::new("0".to_string());
            if node.wallets.valid_password(&wallet_id).unwrap() {
                wallet_locked.locked = "1".to_string();
            }
            to_string_pretty(&wallet_locked).unwrap()
        }
        Err(_) => format_error_message("Bad wallet"),
    }
}
