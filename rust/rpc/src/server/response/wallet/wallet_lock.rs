use crate::format_error_message;
use rsnano_core::WalletId;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct WalletLock {
    lock: String,
}

impl WalletLock {
    fn new(lock: String) -> Self {
        Self { lock }
    }
}

pub(crate) async fn wallet_lock(node: Arc<Node>, wallet: String) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet_id) => {
            node.wallets.lock(&wallet_id).unwrap();
            to_string_pretty(&WalletLock::new("1".to_string())).unwrap()
        }
        Err(_) => format_error_message("Bad wallet"),
    }
}
