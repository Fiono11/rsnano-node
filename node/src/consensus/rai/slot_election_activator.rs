use std::sync::{Arc, Mutex, RwLock};

use rsnano_ledger::{BlockSource, LedgerEvent, ProcessResult};
use rsnano_messages::Message;
use rsnano_network::TrafficType;
use rsnano_types::{BlockHash, RaiElectionId, RaiElectionValue, RaiSlot, RaiVote, SavedBlock};
use rsnano_utils::EventHandler;

use super::{RaiActiveElections, RaiCloseState, RaiElectionInsertError, RaiVoteProcessor};
use crate::{
    block_processing::LedgerPipelineEvent, transport::MessageFlooder,
    wallets::WalletRepresentatives,
};

pub struct RaiSlotElectionActivator {
    active_elections: Arc<RaiActiveElections>,
    close_state: Arc<RwLock<RaiCloseState>>,
    vote_processor: Arc<RaiVoteProcessor>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    message_flooder: Arc<Mutex<MessageFlooder>>,
}

impl RaiSlotElectionActivator {
    pub fn new(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        vote_processor: Arc<RaiVoteProcessor>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        message_flooder: Arc<Mutex<MessageFlooder>>,
    ) -> Self {
        Self {
            active_elections,
            close_state,
            vote_processor,
            wallet_reps,
            message_flooder,
        }
    }

    fn activate_processed(&self, results: &[ProcessResult]) {
        for result in results {
            if matches!(
                result.source,
                BlockSource::Live | BlockSource::LiveOriginator
            ) && result.status.is_ok()
                && let Some(block) = result.saved_block.as_ref()
            {
                self.activate_block(block);
            }
        }
    }

    fn activate_block(&self, block: &SavedBlock) {
        let slot = RaiSlot::new(block.account(), block.height());
        let epoch = self.close_state.read().unwrap().current_epoch();

        if !self
            .close_state
            .read()
            .unwrap()
            .is_slot_vote_enabled(epoch, &slot)
        {
            return;
        }

        let election_id = RaiElectionId::Slot { slot, epoch };
        match self.active_elections.insert(election_id.clone()) {
            Ok(()) | Err(RaiElectionInsertError::Duplicate) => {
                self.publish_local_first_votes(election_id, block.hash());
            }
            Err(RaiElectionInsertError::Stopped) => {}
        }
    }

    fn publish_local_first_votes(&self, election_id: RaiElectionId, block_hash: BlockHash) {
        let mut rep_keys = Vec::new();
        self.wallet_reps
            .lock()
            .unwrap()
            .rep_priv_keys(&mut rep_keys);

        for key in rep_keys {
            let vote = RaiVote::new_first(
                &key,
                election_id.clone(),
                RaiElectionValue::Block(block_hash),
            );
            if self.vote_processor.process(&vote).is_ok() {
                self.message_flooder.lock().unwrap().flood(
                    &Message::RaiVote(vote),
                    TrafficType::Generic,
                    1.0,
                );
            }
        }
    }
}

impl EventHandler<LedgerPipelineEvent> for RaiSlotElectionActivator {
    fn handle(&self, event: &LedgerPipelineEvent) {
        if let LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(results)) = event {
            self.activate_processed(results);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::representatives::RepresentativeTracker;
    use rsnano_ledger::{BlockError, BlockSource, RepWeightCache};
    use rsnano_types::{Block, BlockPriority};
    use rsnano_utils::stats::Stats;
    use rsnano_wallet::Wallets;

    #[test]
    fn successful_processed_block_starts_slot_election() {
        let fixture = Fixture::new();
        let block = SavedBlock::new_test_instance();
        let event =
            LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(vec![processed_block(
                &block,
                Ok(()),
            )]));

        fixture.activator.handle(&event);

        let election_id = slot_election_id(&block);
        assert!(fixture.active_elections.contains(&election_id));
    }

    #[test]
    fn failed_processed_block_does_not_start_slot_election() {
        let fixture = Fixture::new();
        let block = SavedBlock::new_test_instance();
        let event =
            LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(vec![processed_block(
                &block,
                Err(BlockError::BadSignature),
            )]));

        fixture.activator.handle(&event);

        let election_id = slot_election_id(&block);
        assert!(!fixture.active_elections.contains(&election_id));
    }

    #[test]
    fn local_processed_block_does_not_start_slot_election() {
        let fixture = Fixture::new();
        let block = SavedBlock::new_test_instance();
        let event =
            LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(vec![processed_block_from(
                &block,
                BlockSource::Local,
                Ok(()),
            )]));

        fixture.activator.handle(&event);

        let election_id = slot_election_id(&block);
        assert!(!fixture.active_elections.contains(&election_id));
    }

    #[test]
    fn live_processed_block_does_not_start_slot_election_while_closing_before_cut() {
        let fixture = Fixture::new();
        let block = SavedBlock::new_test_instance();
        fixture
            .close_state
            .write()
            .unwrap()
            .start_closing(0)
            .unwrap();
        let event =
            LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(vec![processed_block(
                &block,
                Ok(()),
            )]));

        fixture.activator.handle(&event);

        let election_id = slot_election_id(&block);
        assert!(!fixture.active_elections.contains(&election_id));
    }

    struct Fixture {
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        activator: RaiSlotElectionActivator,
    }

    impl Fixture {
        fn new() -> Self {
            let active_elections = Arc::new(RaiActiveElections::new());
            let close_state = Arc::new(RwLock::new(RaiCloseState::new()));
            let rep_tracker = Arc::new(RepresentativeTracker::new_null());
            let vote_processor = Arc::new(RaiVoteProcessor::with_committee_provider(
                active_elections.clone(),
                close_state.clone(),
                rep_tracker.clone(),
                Arc::new(super::super::RepWeightRaiCommitteeProvider::new(Arc::new(
                    RepWeightCache::default(),
                ))),
                Arc::new(Stats::default()),
            ));
            let wallet_reps = Arc::new(Mutex::new(WalletRepresentatives::new(
                false,
                Default::default(),
                Arc::new(RepWeightCache::default()),
                Arc::new(Wallets::new_null()),
                rep_tracker,
            )));
            let message_flooder = Arc::new(Mutex::new(MessageFlooder::new_null()));
            let activator = RaiSlotElectionActivator::new(
                active_elections.clone(),
                close_state.clone(),
                vote_processor,
                wallet_reps,
                message_flooder,
            );

            Self {
                active_elections,
                close_state,
                activator,
            }
        }
    }

    fn processed_block(block: &SavedBlock, status: Result<(), BlockError>) -> ProcessResult {
        processed_block_from(block, BlockSource::Live, status)
    }

    fn processed_block_from(
        block: &SavedBlock,
        source: BlockSource,
        status: Result<(), BlockError>,
    ) -> ProcessResult {
        ProcessResult {
            block: Block::from(block.clone()),
            source,
            status,
            saved_block: Some(block.clone()),
            priority: BlockPriority::default(),
        }
    }

    fn slot_election_id(block: &SavedBlock) -> RaiElectionId {
        RaiElectionId::Slot {
            slot: RaiSlot::new(block.account(), block.height()),
            epoch: 0,
        }
    }
}
