use crate::server::service::format_error_message;
use rsnano_core::{Account, PublicKey, WalletId};
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct AccountMove {
    moved: String,
}

impl AccountMove {
    fn new(moved: String) -> Self {
        Self { moved }
    }
}

pub(crate) async fn account_move(
    node: Arc<Node>,
    wallet: String,
    source: String,
    accounts: Vec<String>,
) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet_id) => match WalletId::decode_hex(&source) {
            Ok(target_id) => match WalletId::decode_hex(&wallet) {
                Ok(_) => {
                    let decoded_accounts: Vec<PublicKey> = accounts
                        .iter()
                        .map(|str| Account::decode_hex(str).unwrap())
                        .collect();

                    match node
                        .wallets
                        .move_accounts(&wallet_id, &target_id, &decoded_accounts)
                    {
                        Ok(_) => to_string_pretty(&AccountMove::new(String::from("1"))).unwrap(),
                        Err(e) => format_error_message(&format!("Failed to move accounts: {}", e)),
                    }
                }
                Err(_) => format_error_message("Bad target account number"),
            },
            Err(_) => format_error_message("Bad source account number"),
        },
        Err(_) => format_error_message("Bad wallet ID"),
    }
}
