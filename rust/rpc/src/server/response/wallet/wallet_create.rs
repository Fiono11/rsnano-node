use crate::format_error_message;
use rand::{thread_rng, Rng};
use rsnano_core::WalletId;
use rsnano_node::{node::Node, wallets::WalletsExt};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct WalletCreate {
    wallet: String,
}

impl WalletCreate {
    fn new(wallet: String) -> Self {
        Self { wallet }
    }
}

pub(crate) async fn wallet_create(node: Arc<Node>, seed: Option<String>) -> String {
    let wallet_id = match seed {
        Some(seed_value) => match WalletId::decode_hex(seed_value) {
            Ok(wallet_id) => wallet_id,
            Err(_) => return format_error_message("Invalid seed"),
        },
        None => WalletId::from_bytes(thread_rng().gen()),
    };

    node.wallets.create(wallet_id);

    to_string_pretty(&WalletCreate::new(wallet_id.encode_hex())).unwrap()
}
