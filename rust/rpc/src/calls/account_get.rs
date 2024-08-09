use crate::server::Service;
use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountGet {
    account: String,
}

impl Service {
    pub async fn account_get(&self, key: String) -> String {
        match Account::decode_hex(&key) {
            Ok(pk) => {
                let account_get = AccountGet {
                    account: pk.encode_account(),
                };
                to_string_pretty(&account_get).unwrap()
            }
            Err(_) => {
                let error = json!({ "error": "Bad public key" });
                to_string_pretty(&error).unwrap()
            }
        }
    }
}
