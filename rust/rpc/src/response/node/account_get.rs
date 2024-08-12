use rsnano_core::Account;
use serde::Serialize;
use serde_json::to_string_pretty;

use crate::format_error_message;

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
        Err(_) => format_error_message("Bad public key"),
    }
}
