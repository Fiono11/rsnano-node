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
    work: Option<String>,
) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet) => match RawKey::decode_hex(&key) {
            Ok(raw_key) => {
                let generate_work = work
                    .as_ref()
                    .map(|w| w.parse::<bool>().unwrap_or(false))
                    .unwrap_or(false);

                match node.wallets.insert_adhoc2(&wallet, &raw_key, generate_work) {
                    Ok(account) => to_string_pretty(&WalletAdd::new(account.encode_account()))
                        .unwrap_or_else(|_| format_error_message("Serialization error")),
                    Err(_) => format_error_message("Failed to add key to wallet"),
                }
            }
            Err(_) => format_error_message("Bad key"),
        },
        Err(_) => format_error_message("Bad wallet"),
    }
}
