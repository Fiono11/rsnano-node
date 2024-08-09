use crate::server::Service;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountKey {
    key: String,
}

impl Service {
    pub async fn account_key(&self, account: String) -> String {
        match Account::decode_account(&account) {
            Ok(account) => {
                let account_key = AccountKey {
                    key: account.encode_hex(),
                };
                to_string_pretty(&account_key).unwrap()
            }
            Err(_) => {
                let error = json!({ "error": "Bad account number" });
                to_string_pretty(&error).unwrap()
            }
        }
    }
}
