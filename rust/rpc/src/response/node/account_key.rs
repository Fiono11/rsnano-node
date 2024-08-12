use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountKey {
    key: String,
}

impl AccountKey {
    fn new(key: String) -> Self {
        Self { key }
    }
}

pub(crate) async fn account_key(account: String) -> String {
    match Account::decode_account(&account) {
        Ok(account) => to_string_pretty(&AccountKey::new(account.encode_hex())).unwrap(),
        Err(_) => to_string_pretty(&json!({ "error": "Bad account number" })).unwrap(),
    }
}
