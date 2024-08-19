use crate::format_error_message;
use rsnano_core::WalletId;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct WalletDestroy {
    destroyed: String,
}

impl WalletDestroy {
    fn new(destroyed: String) -> Self {
        Self { destroyed }
    }
}

pub(crate) async fn wallet_destroy(node: Arc<Node>, wallet: String) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(id) => {
            node.wallets.destroy(&id);
            to_string_pretty(&WalletDestroy::new("1".to_string())).unwrap()
        }
        Err(_) => format_error_message("Bad wallet"),
    }
}
