use rsnano_core::Account;
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct AccountGet {
    account: String,
}

impl AccountGet {
    fn new(account: String) -> Self {
        Self { account }
    }
}

pub(crate) async fn account_get(key: String) -> String {
    match Account::decode_hex(&key) {
        Ok(pk) => to_string_pretty(&AccountGet::new(pk.encode_account())).unwrap(),
        Err(_) => to_string_pretty(&json!({ "error": "Bad public key" })).unwrap(),
    }
}
