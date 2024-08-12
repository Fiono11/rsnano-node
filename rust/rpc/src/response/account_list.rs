use crate::format_error_message;
use rsnano_core::WalletId;
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct AccountList {
    accounts: Vec<String>,
}

impl AccountList {
    fn new(accounts: Vec<String>) -> Self {
        Self { accounts }
    }
}

pub(crate) async fn account_list(node: Arc<Node>, wallet: String) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet_id) => match node.wallets.get_accounts_of_wallet(&wallet_id) {
            Ok(accounts) => {
                let account_list =
                    AccountList::new(accounts.iter().map(|pk| pk.encode_account()).collect());
                to_string_pretty(&account_list).unwrap()
            }
            Err(_) => format_error_message("Wallet not found"),
        },
        Err(_) => format_error_message("Bad wallet"),
    }
}
