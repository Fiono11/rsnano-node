use crate::format_error_message;
use rsnano_core::{Account, WalletId};
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct WalletContains {
    exists: String,
}

impl WalletContains {
    fn new(exists: String) -> Self {
        Self { exists }
    }
}

pub(crate) async fn wallet_contains(node: Arc<Node>, wallet: String, account: String) -> String {
    // Decode the wallet ID
    let wallet_id = match WalletId::decode_hex(&wallet) {
        Ok(id) => id,
        Err(_) => return format_error_message("Bad wallet"),
    };

    // Decode the account
    let accounts = match Account::decode_account(&account) {
        Ok(acct) => acct,
        Err(_) => return format_error_message("Bad account"),
    };

    // Get accounts of the wallet
    let wallet_accounts = match node.wallets.get_accounts_of_wallet(&wallet_id) {
        Ok(acc) => acc,
        Err(_) => return format_error_message("Failed to get accounts of wallet"),
    };

    // Check if the account exists in the wallet
    let mut wallet_contains = WalletContains::new("0".to_string());
    if wallet_accounts.contains(&accounts) {
        wallet_contains.exists = "1".to_string();
    }

    to_string_pretty(&wallet_contains).unwrap()
}
