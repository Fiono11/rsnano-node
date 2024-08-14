use crate::server::service::format_error_message;
use rsnano_core::{Account, BlockEnum, WalletId};
use rsnano_node::{node::Node, wallets::WalletsExt};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::{Arc, Condvar, Mutex};

#[derive(Serialize)]
struct AccountRepresentativeSet {
    block: String,
}

impl AccountRepresentativeSet {
    fn new(block: String) -> Self {
        Self { block }
    }
}

pub(crate) async fn account_representative_set(
    node: Arc<Node>,
    wallet: String,
    account: String,
    representative: String,
    work: Option<bool>,
) -> String {
    match WalletId::decode_hex(&wallet) {
        Ok(wallet) => match Account::decode_account(&account) {
            Ok(account) => match Account::decode_account(&representative) {
                Ok(representative) => {
                    let result = Arc::new((Condvar::new(), Mutex::new((false, None))));
                    let result_clone = Arc::clone(&result);

                    let change_async_result = node.wallets.change_async(
                        wallet,
                        account,
                        representative,
                        Box::new(move |block| {
                            *result_clone.1.lock().unwrap() = (true, block);
                            result_clone.0.notify_all();
                        }),
                        0,
                        work.unwrap_or(true),
                    );

                    if change_async_result.is_err() {
                        return format_error_message(
                            "Failed to initiate account representative change",
                        );
                    }

                    let block: Option<BlockEnum> = {
                        let (ref condvar, ref mutex) = *result;
                        let mut result_guard = mutex.lock().unwrap();
                        while !result_guard.0 {
                            result_guard = condvar.wait(result_guard).unwrap();
                        }
                        result_guard.1.clone()
                    };

                    if let Some(block) = block {
                        to_string_pretty(&AccountRepresentativeSet::new(block.hash().encode_hex()))
                            .unwrap()
                    } else {
                        format_error_message("Failed to set account representative")
                    }
                }
                Err(_) => format_error_message("Bad representative"),
            },
            Err(_) => format_error_message("Bad account number"),
        },
        Err(_) => format_error_message("Bad wallet"),
    }
}
