use std::sync::Arc;

use crate::server::{json_error, RpcRequest, Service};
use anyhow::Result;
use rsnano_core::BlockHash;
use rsnano_node::consensus::{ElectionStatus, ElectionStatusType};
use serde::Serialize;
use serde_json::{json, to_string_pretty};

#[derive(Serialize)]
struct BlockConfirm {
    started: String,
}

impl BlockConfirm {
    fn new(started: String) -> Self {
        Self { started }
    }
}

impl Service {
    pub(crate) async fn block_confirm(&self, hash_str: String) -> String {
        let tx = self.node.ledger.read_txn();
        match BlockHash::decode_hex(&hash_str) {
            Ok(hash) => match &self.node.ledger.any().get_block(&tx, &hash) {
                Some(block) => {
                    if !self
                        .node
                        .ledger
                        .confirmed()
                        .block_exists_or_pruned(&tx, &hash)
                    {
                        // Start new confirmation for unconfirmed (or not being confirmed) block
                        if !self.node.confirming_set.exists(&hash) {
                            self.node
                                .manual_scheduler
                                .push(Arc::new(block.clone()), None);
                        }
                    } else {
                        // Add record in confirmation history for confirmed block
                        let mut status = ElectionStatus::default();
                        status.winner = Some(Arc::new(block.clone()));
                        status.election_end = std::time::SystemTime::now();
                        status.block_count = 1;
                        status.election_status_type = ElectionStatusType::ActiveConfirmationHeight;
                        self.node.active.insert_recently_cemented(status);
                    }
                    let block_confirm = BlockConfirm::new("1".to_string());
                    to_string_pretty(&block_confirm).unwrap()
                }
                None => to_string_pretty(&json!({ "error": "Block not found" })).unwrap(),
            },
            Err(_) => to_string_pretty(&json!({ "error": "Invalid block hash" })).unwrap(),
        }
    }
}

pub(crate) async fn handle_block_confirm(
    service: &Service,
    rpc_request: RpcRequest,
) -> Result<String> {
    if let Some(hash) = rpc_request.hash {
        Ok(service.block_confirm(hash).await)
    } else {
        Err(json_error("Unable to parse JSON"))
    }
}
