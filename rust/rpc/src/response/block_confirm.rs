use crate::format_error_message;
use rsnano_core::BlockHash;
use rsnano_node::{
    consensus::{ElectionStatus, ElectionStatusType},
    node::Node,
};
use serde::Serialize;
use serde_json::to_string_pretty;
use std::sync::Arc;

#[derive(Serialize)]
struct BlockConfirm {
    started: String,
}

impl BlockConfirm {
    fn new(started: String) -> Self {
        Self { started }
    }
}

pub(crate) async fn block_confirm(node: Arc<Node>, hash_str: String) -> String {
    let tx = node.ledger.read_txn();
    match BlockHash::decode_hex(&hash_str) {
        Ok(hash) => match &node.ledger.any().get_block(&tx, &hash) {
            Some(block) => {
                if !node.ledger.confirmed().block_exists_or_pruned(&tx, &hash) {
                    // Start new confirmation for unconfirmed (or not being confirmed) block
                    if !node.confirming_set.exists(&hash) {
                        node.manual_scheduler.push(Arc::new(block.clone()), None);
                    }
                } else {
                    // Add record in confirmation history for confirmed block
                    let mut status = ElectionStatus::default();
                    status.winner = Some(Arc::new(block.clone()));
                    status.election_end = std::time::SystemTime::now();
                    status.block_count = 1;
                    status.election_status_type = ElectionStatusType::ActiveConfirmationHeight;
                    node.active.insert_recently_cemented(status);
                }
                let block_confirm = BlockConfirm::new("1".to_string());
                to_string_pretty(&block_confirm).unwrap()
            }
            None => format_error_message("Block not found"),
        },
        Err(_) => format_error_message("Invalid block hash"),
    }
}
