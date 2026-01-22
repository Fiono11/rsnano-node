use std::sync::{Arc, Mutex};

use crate::{
    block_processing::LedgerEvent,
    ledger_event_processor::LedgerEventProcessorPlugin,
    ledger_snapshots::LedgerSnapshots,
};
use tracing::{info, warn};

/// Plugin that triggers ledger snapshots every N confirmed blocks
pub(crate) struct SnapshotTriggerPlugin {
    ledger_snapshots: Arc<LedgerSnapshots>,
    confirmed_count: Mutex<u64>,
    threshold: u64,
}

impl SnapshotTriggerPlugin {
    pub(crate) fn new(ledger_snapshots: Arc<LedgerSnapshots>, threshold: u64) -> Self {
        info!(
            threshold = threshold,
            "Initializing snapshot trigger plugin: will trigger snapshots every {} confirmed blocks",
            threshold
        );
        Self {
            ledger_snapshots,
            confirmed_count: Mutex::new(0),
            threshold,
        }
    }
}

impl LedgerEventProcessorPlugin for SnapshotTriggerPlugin {
    fn process(&mut self, event: &LedgerEvent) {
        if let LedgerEvent::BlocksConfirmed(confirmed) = event {
            let count = confirmed.len() as u64;
            let mut total_count = self.confirmed_count.lock().unwrap();
            *total_count += count;

            info!(
                blocks_confirmed = count,
                total_since_last_snapshot = *total_count,
                threshold = self.threshold,
                "Blocks confirmed: {} new blocks, {} total since last snapshot",
                count,
                *total_count
            );

            if *total_count >= self.threshold {
                warn!(
                    total_confirmed = *total_count,
                    threshold = self.threshold,
                    "Threshold reached! Triggering ledger snapshot"
                );
                self.ledger_snapshots.start_ledger_snapshot();
                *total_count = 0;
                info!(
                    "Snapshot triggered, counter reset to 0"
                );
            }
        }
    }
}
