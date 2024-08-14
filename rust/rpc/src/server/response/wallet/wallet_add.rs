use crate::server::service::format_error_message;
use rsnano_core::{RawKey, WalletId};
use rsnano_node::{node::Node, wallets::WalletsExt};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct WalletAdd {
    account: String,
}

impl WalletAdd {
    fn new(account: String) -> Self {
        Self { account }
    }
}

pub(crate) async fn wallet_add(
    node: Arc<Node>,
    wallet: String,
    key: String,
    work: Option<bool>,
) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet) => {
            let account = node
                .wallets
                .insert_adhoc2(
                    &wallet,
                    &RawKey::decode_hex(&key).unwrap(),
                    work.unwrap_or(false),
                )
                .unwrap();
            to_string_pretty(&WalletAdd::new(account.encode_account())).unwrap()
        }
        Err(_) => format_error_message("Bad wallet"),
    }
}
