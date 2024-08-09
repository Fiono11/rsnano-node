use crate::server::Service;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountBlockCount {
    block_count: String,
}

impl Service {
    pub async fn account_block_count(&self, account_str: String) -> String {
        let tx = self.node.ledger.read_txn();
        match Account::decode_account(&account_str) {
            Ok(account) => match self.node.ledger.store.account.get(&tx, &account) {
                Some(account_info) => {
                    let account_block_count = AccountBlockCount {
                        block_count: account_info.block_count.to_string(),
                    };
                    to_string_pretty(&account_block_count).unwrap()
                }
                None => {
                    let error = json!({ "error": "Account not found" });
                    to_string_pretty(&error).unwrap()
                }
            },
            Err(_) => {
                let error = json!({ "error": "Bad account number" });
                to_string_pretty(&error).unwrap()
            }
        }
    }
}
