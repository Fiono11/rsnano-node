use crate::format_error_message;
use rsnano_core::{Account, WalletId};
use rsnano_node::{node::Node, wallets::WalletsExt};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct AccountsCreate {
    accounts: Vec<String>,
}

impl AccountsCreate {
    fn new(accounts: Vec<String>) -> Self {
        Self { accounts }
    }
}

pub(crate) async fn accounts_create(node: Arc<Node>, wallet: String, count: String) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet) => match count.parse::<u32>() {
            Ok(count) => {
                let mut accounts = vec![];
                for _ in 0..count {
                    match node.wallets.deterministic_insert2(&wallet, false) {
                        Ok(public_key) => {
                            let account = Account::encode_account(&public_key);
                            accounts.push(account);
                        }
                        Err(_) => return format_error_message("Failed to create account"),
                    }
                }
                to_string_pretty(&AccountsCreate::new(accounts))
                    .unwrap_or_else(|_| format_error_message("Serialization error"))
            }
            Err(_) => format_error_message("Invalid count"),
        },
        Err(_) => format_error_message("Bad wallet"),
    }
}
