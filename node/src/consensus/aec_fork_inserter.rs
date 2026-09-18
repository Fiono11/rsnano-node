use std::sync::{Arc, Mutex, RwLock};

use tracing::debug;

use rsnano_ledger::{BlockError, LedgerEvent, ProcessResult};
use rsnano_messages::{Message, Publish};
use rsnano_network::TrafficType;
use rsnano_types::{Block, QualifiedRoot};
use rsnano_utils::EventHandlerMut;

use super::{AecService, ForkCache};
use crate::{
    block_processing::LedgerPipelineEvent, consensus::vote_cache::VoteCache,
    transport::MessageFlooder,
};

pub(crate) struct AecForkInserter {
    pub(crate) fork_cache: Arc<RwLock<ForkCache>>,
    pub(crate) active_elections: Arc<AecService>,
    pub(crate) vote_cache: Arc<VoteCache>,
    pub(crate) message_flooder: Arc<Mutex<MessageFlooder>>,
}

impl AecForkInserter {
    #[allow(dead_code)]
    pub fn new_test_instance() -> Self {
        Self {
            fork_cache: Arc::new(RwLock::new(ForkCache::new())),
            active_elections: Arc::new(AecService::new_null()),
            vote_cache: Arc::new(VoteCache::new_null()),
            message_flooder: Arc::new(Mutex::new(MessageFlooder::new_null())),
        }
    }

    pub fn handle_forks(&self, batch: &[ProcessResult]) {
        for result in batch {
            if result.status == Err(BlockError::Fork) {
                self.handle_fork(&result.block);
            }
        }
    }

    pub fn try_add_cached_forks(&self, root: &QualifiedRoot) {
        let fork_cache = self.fork_cache.read().unwrap();
        for fork in fork_cache.get_forks(root) {
            self.handle_fork(fork);
        }
    }

    fn handle_fork(&self, fork: &Block) {
        let fork_tally = self.vote_cache.get_non_final_tally(&fork.hash());
        let added = self.active_elections.try_add_fork(fork, fork_tally);

        if added {
            debug!("Block was added to an existing election: {}", fork.hash());
            if cfg!(feature = "rai_protocol") {
                // Kudzu ships the payload with the votes, so every replica can take
                // its second look. Here the candidate is flooded to the principal
                // representatives instead; forks are rare, so this is cheap.
                let publish = Message::Publish(Publish::new_forward(fork.clone()));
                self.message_flooder
                    .lock()
                    .unwrap()
                    .flood_prs_and_some_non_prs(&publish, TrafficType::BlockBroadcastInitial, 0.5);
            }
        }
    }
}

pub(crate) struct ForkInserterPlugin {
    fork_processor: Arc<AecForkInserter>,
}

impl ForkInserterPlugin {
    pub fn new(fork_processor: Arc<AecForkInserter>) -> Self {
        Self { fork_processor }
    }
}

impl EventHandlerMut<LedgerPipelineEvent> for ForkInserterPlugin {
    fn handle(&mut self, event: &LedgerPipelineEvent) {
        if let LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(results)) = event {
            // Notify elections about alternative (forked) blocks
            self.fork_processor.handle_forks(results);
        }
    }
}
