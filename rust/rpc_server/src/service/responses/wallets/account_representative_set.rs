use rsnano_core::{Account, BlockEnum, WalletId};
use rsnano_node::{node::Node, wallets::WalletsExt};
use rsnano_rpc_messages::AccountRepresentativeSetDto;
use serde_json::to_string_pretty;
use std::sync::{Arc, Condvar, Mutex};

use crate::service::responses::format_error_message;

pub async fn account_representative_set(
    node: Arc<Node>,
    wallet: WalletId,
    account: Account,
    representative: Account,
    work: Option<u64>,
) -> String {
    // Check if work is needed
    let generate_work = work.unwrap_or(1) != 0;

    // Shared state to wait for the async operation
    let result = Arc::new((Condvar::new(), Mutex::new((false, None))));
    let result_clone = Arc::clone(&result);

    // Initiate the async operation
    let change_async_result = node.wallets.change_async(
        wallet,
        account,
        representative.into(),
        Box::new(move |block| {
            let mut result_guard = result_clone.1.lock().unwrap();
            *result_guard = (true, Some(block));
            result_clone.0.notify_all();
        }),
        0,
        generate_work,
    );

    // Check if initiation of the change was successful
    if change_async_result.is_err() {
        return format_error_message("Failed to initiate account representative change");
    }

    // Wait for the block to be processed
    let block: Option<BlockEnum> = {
        let (ref condvar, ref mutex) = *result;
        let mut result_guard = mutex.lock().unwrap();
        while !result_guard.0 {
            result_guard = condvar.wait(result_guard).unwrap();
        }
        result_guard.1.take().flatten()
    };

    // Check if the block is set, and return the response
    if let Some(block) = block {
        let block_hash = block.hash();
        to_string_pretty(&AccountRepresentativeSetDto::new(block_hash))
            .unwrap_or_else(|_| format_error_message("Serialization error"))
    } else {
        format_error_message("Failed to set account representative")
    }
}
