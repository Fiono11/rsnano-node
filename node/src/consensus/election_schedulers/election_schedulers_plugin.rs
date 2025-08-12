use std::sync::Arc;

use crate::{block_processing::LedgerEvent, ledger_event_processor::LedgerEventProcessorPlugin};

use super::ElectionSchedulers;

pub(crate) struct ElectionSchedulersPlugin {
    schedulers: Arc<ElectionSchedulers>,
}

impl ElectionSchedulersPlugin {
    pub(crate) fn new(schedulers: Arc<ElectionSchedulers>) -> Self {
        Self { schedulers }
    }
}

impl LedgerEventProcessorPlugin for ElectionSchedulersPlugin {
    fn process(&mut self, event: &LedgerEvent) {
        match event {
            LedgerEvent::BlocksProcessed(results) => {
                // Check for preordering blocks and notify the ordering scheduler
                for result in results {
                    if result.status.is_ok() {
                        if let Some(saved_block) = &result.saved_block {
                            if saved_block.as_block().block_type() == rsnano_core::BlockType::PreOrdering {
                                // This is a preordering block, notify the ordering scheduler
                                self.schedulers.ordering.on_preordering_block_received(saved_block.clone());
                            }
                        }
                    }
                }

                self.schedulers
                    .activate_accounts_with_fresh_blocks(&results);
            }
            LedgerEvent::BlocksConfirmed(confirmed) => {
                // Extract block hashes for ordering scheduler
                let block_hashes: Vec<rsnano_core::BlockHash> =
                    confirmed.iter().map(|(block, _)| block.hash()).collect();

                // Notify the ordering scheduler about confirmed blocks
                self.schedulers
                    .ordering
                    .on_blocks_confirmed(confirmed.len());

                // Notify with block hashes
                self.schedulers
                    .ordering
                    .on_blocks_confirmed_with_hashes(&block_hashes);

                // Activate successors for other schedulers
                self.schedulers
                    .activate_successors(confirmed.iter().map(|(b, _)| b));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_core::{BlockHash, SavedBlock};

    #[test]
    fn when_blocks_confirmed_should_activate_elections_for_sucessors() {
        let schedulers = Arc::new(ElectionSchedulers::new_null());
        let mut processor = ElectionSchedulersPlugin::new(schedulers.clone());
        let activation_tracker = schedulers.track_activate_successors();

        let block = SavedBlock::new_test_instance();
        let confirmed_blocks = vec![(block.clone(), BlockHash::from(123))];
        processor.process(&LedgerEvent::BlocksConfirmed(confirmed_blocks));

        let output = activation_tracker.output();
        assert_eq!(output, [block]);
    }

    #[test]
    fn committed_count_increases_when_blocks_confirmed() {
        let schedulers = Arc::new(ElectionSchedulers::new_null());
        let mut processor = ElectionSchedulersPlugin::new(schedulers.clone());

        let block = SavedBlock::new_test_instance();
        let confirmed_blocks = vec![(block.clone(), BlockHash::from(123))];
        processor.process(&LedgerEvent::BlocksConfirmed(confirmed_blocks));

        assert_eq!(schedulers.ordering.committed_count(), 1);
    }
}
