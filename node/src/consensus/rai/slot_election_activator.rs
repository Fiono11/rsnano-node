use std::sync::{Arc, Mutex, RwLock};

use rsnano_ledger::{BlockSource, LedgerEvent, ProcessResult};
use rsnano_messages::Message;
use rsnano_network::TrafficType;
use rsnano_types::{BlockHash, RaiElectionId, RaiElectionValue, RaiSlot, RaiVote, SavedBlock};
use rsnano_utils::EventHandler;

use super::{RaiActiveElections, RaiCloseState, RaiElectionInsertError, RaiVoteProcessor};
use crate::consensus::election_schedulers::priority::PriorityElectionActivator;
use crate::{
    block_processing::LedgerPipelineEvent, transport::MessageFlooder,
    wallets::WalletRepresentatives,
};
use rsnano_utils::thread_pool::ThreadPool;

#[derive(Clone)]
pub struct RaiSlotElectionActivator {
    active_elections: Arc<RaiActiveElections>,
    close_state: Arc<RwLock<RaiCloseState>>,
    vote_processor: Arc<RaiVoteProcessor>,
    wallet_reps: Arc<Mutex<WalletRepresentatives>>,
    message_flooder: Arc<Mutex<MessageFlooder>>,
    executor: Option<Arc<ThreadPool>>,
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
            executor: None,
        }
    }

    pub fn new_async(
        active_elections: Arc<RaiActiveElections>,
        close_state: Arc<RwLock<RaiCloseState>>,
        vote_processor: Arc<RaiVoteProcessor>,
        wallet_reps: Arc<Mutex<WalletRepresentatives>>,
        message_flooder: Arc<Mutex<MessageFlooder>>,
        executor: Arc<ThreadPool>,
    ) -> Self {
        Self {
            active_elections,
            close_state,
            vote_processor,
            wallet_reps,
            message_flooder,
            executor: Some(executor),
        }
    }

    fn activate_processed(&self, results: &[ProcessResult]) {
        for result in results {
            if matches!(
                result.source,
                BlockSource::Local | BlockSource::Live | BlockSource::LiveOriginator
            ) && result.status.is_ok()
                && let Some(block) = result.saved_block.as_ref()
            {
                self.activate_block(block);
            }
        }
    }

    pub fn activate_block(&self, block: &SavedBlock) -> bool {
        let slot = RaiSlot::new(block.account(), block.height());
        let epoch = self.close_state.read().unwrap().open_epoch();

        if !self
            .close_state
            .read()
            .unwrap()
            .is_slot_vote_enabled(epoch, &slot)
        {
            return false;
        }

        let election_id = RaiElectionId::Slot { slot, epoch };
        match self.active_elections.insert(election_id.clone()) {
            Ok(()) => self.publish_local_first_votes(election_id, block.hash()),
            // Another activation path already owns this election.  Treat the
            // scheduler candidate as consumed; republishing our first vote here
            // would only be a replay and cause the scheduler to requeue forever.
            Err(RaiElectionInsertError::Duplicate) => true,
            Err(RaiElectionInsertError::Stopped) => false,
        }
    }

    fn publish_local_first_votes(&self, election_id: RaiElectionId, block_hash: BlockHash) -> bool {
        let mut rep_keys = Vec::new();
        let wallet_reps = self.wallet_reps.lock().unwrap();
        wallet_reps.rep_priv_keys(&mut rep_keys);
        if rep_keys.is_empty() {
            rep_keys = wallet_reps.voting_priv_keys_unfiltered();
        }
        drop(wallet_reps);

        let mut published = false;
        for key in rep_keys {
            let value = RaiElectionValue::Block(block_hash);
            // A first vote already contributes notarization weight.  A separate
            // notarization (second-look) vote and a final vote are state-machine
            // transitions and must only be generated after their predicates have
            // become enabled by observed votes.
            let vote = RaiVote::new_first(&key, election_id.clone(), value);
            if self.vote_processor.process(&vote).is_ok() {
                self.message_flooder.lock().unwrap().flood(
                    &Message::RaiVote(vote),
                    TrafficType::Generic,
                    1.0,
                );
                published = true;
            }
        }
        published
    }
}

impl PriorityElectionActivator for RaiSlotElectionActivator {
    fn vacancy(&self) -> usize {
        5_000usize.saturating_sub(self.active_elections.len())
    }

    fn activate(&self, block: SavedBlock) -> bool {
        self.activate_block(&block)
    }
}

impl EventHandler<LedgerPipelineEvent> for RaiSlotElectionActivator {
    fn handle(&self, event: &LedgerPipelineEvent) {
        if let LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(results)) = event {
            if let Some(executor) = &self.executor {
                let this = self.clone();
                let results = results.clone();
                executor.execute(move || this.activate_processed(&results));
            } else {
                self.activate_processed(results);
            }
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
    fn live_processed_block_starts_slot_election() {
        let fixture = Fixture::new();
        let block = SavedBlock::new_test_instance();
        let event =
            LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(vec![processed_block_from(
                &block,
                BlockSource::Live,
                Ok(()),
            )]));

        fixture.activator.handle(&event);

        let election_id = slot_election_id(&block);
        assert!(fixture.active_elections.contains(&election_id));
    }

    #[test]
    fn live_processed_block_starts_in_successor_epoch_while_current_epoch_closes() {
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

        let election_id = RaiElectionId::Slot {
            slot: RaiSlot::new(block.account(), block.height()),
            epoch: 1,
        };
        assert!(fixture.active_elections.contains(&election_id));
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
        processed_block_from(block, BlockSource::Local, status)
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
