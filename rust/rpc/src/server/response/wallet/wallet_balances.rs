use crate::server::{response::AccountBalance, service::format_error_message};
use rsnano_core::{Account, WalletId};
use rsnano_node::node::Node;
use serde::Serialize;
use serde_json::to_string_pretty;
use std::{collections::HashMap, sync::Arc};

#[derive(Serialize)]
struct WalletBalances {
    balances: HashMap<Account, AccountBalance>,
}

impl WalletBalances {
    fn new(balances: HashMap<Account, AccountBalance>) -> Self {
        Self { balances }
    }
}

pub(crate) async fn wallet_balances(
    node: Arc<Node>,
    wallet: String,
    threshold: Option<String>,
) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet) => {
            let accounts = node.wallets.get_accounts_of_wallet(&wallet).unwrap();
            let mut balances = HashMap::new();
            let tx = node.ledger.read_txn();
            for account in accounts {
                let balance = match node.ledger.confirmed().account_balance(&tx, &account) {
                    Some(balance) => balance,
                    None => return format_error_message("Account not found"),
                };

                let pending = node.ledger.account_receivable(&tx, &account, true);

                let account_balance = AccountBalance::new(
                    balance.number().to_string(),
                    pending.number().to_string(),
                    pending.number().to_string(),
                );
                balances.insert(account, account_balance);
            }
            to_string_pretty(&WalletBalances::new(balances)).unwrap()
        }
        Err(_) => format_error_message("Bad wallet"),
    }
}
