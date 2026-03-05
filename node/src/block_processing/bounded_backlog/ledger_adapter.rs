use std::sync::Arc;

use rsnano_ledger::LedgerEvent;
use rsnano_utils::EventHandler;

use crate::block_processing::LedgerPipelineEvent;

use super::BoundedBacklog;

/// Makes the bounded backlog react to ledger events
pub(crate) struct BoundedBacklogLedgerAdapter {
    bounded_backlog: Arc<BoundedBacklog>,
}

impl BoundedBacklogLedgerAdapter {
    pub(crate) fn new(bounded_backlog: Arc<BoundedBacklog>) -> Self {
        Self { bounded_backlog }
    }
}

impl EventHandler<LedgerPipelineEvent> for BoundedBacklogLedgerAdapter {
    fn handle(&mut self, event: &LedgerPipelineEvent) {
        if let LedgerPipelineEvent::Ledger(event) = event {
            match event {
                LedgerEvent::BlocksProcessed(results) => {
                    self.bounded_backlog.insert_processed(results);
                }
                LedgerEvent::BlocksConfirmed(confirmed) => {
                    self.bounded_backlog.remove(confirmed);
                }
                LedgerEvent::BlocksRolledBack(rolled_back) => {
                    self.bounded_backlog.erase_hashes(rolled_back.hashes());
                }
            }
        }
    }
}
