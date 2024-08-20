use crate::server::service::format_error_message;
use rsnano_core::{Account, WalletId};
use rsnano_node::{node::Node, wallets::WalletsExt};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct AccountCreate {
    account: String,
}

impl AccountCreate {
    fn new(account: String) -> Self {
        Self { account }
    }
}

pub(crate) async fn account_create(
    node: Arc<Node>,
    wallet: String,
    index: Option<String>,
) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet) => {
            let result = if let Some(index_str) = index {
                match index_str.parse::<u32>() {
                    Ok(i) => node.wallets.deterministic_insert_at(&wallet, i, false),
                    Err(_) => {
                        return format_error_message("Invalid index");
                    }
                }
            } else {
                node.wallets.deterministic_insert2(&wallet, false)
            };

            match result {
                Ok(public_key) => {
                    let account = Account::encode_account(&public_key);
                    to_string_pretty(&AccountCreate::new(account)).unwrap()
                }
                Err(_) => format_error_message("Failed to create account"),
            }
        }
        Err(_) => format_error_message("Bad wallet"),
    }
}
