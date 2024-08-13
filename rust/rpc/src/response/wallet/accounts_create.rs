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

pub(crate) async fn accounts_create(node: Arc<Node>, wallet: String, count: u32) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet) => {
            let mut accounts = vec![];
            for _ in 0..count {
                let public_key = node.wallets.deterministic_insert2(&wallet, false).unwrap();
                let account = Account::encode_account(&public_key);
                accounts.push(account);
            }
            to_string_pretty(&AccountsCreate::new(accounts)).unwrap()
        }
        Err(_) => format_error_message("Bad wallet"),
    }
}
