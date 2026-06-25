use std::sync::{Arc, RwLock};

use tracing::debug;

use rsnano_ledger::{BlockError, LedgerEvent, ProcessResult};
use rsnano_types::{Block, QualifiedRoot};
use rsnano_utils::EventHandlerMut;

use super::{AecService, ForkCache, VoteCache};
use crate::block_processing::LedgerPipelineEvent;

pub(crate) struct AecForkInserter {
    pub(crate) fork_cache: Arc<RwLock<ForkCache>>,
    pub(crate) active_elections: Arc<AecService>,
    pub(crate) vote_cache: Arc<VoteCache>,
}

impl AecForkInserter {
    #[allow(dead_code)]
    pub fn new_test_instance() -> Self {
        Self {
            fork_cache: Arc::new(RwLock::new(ForkCache::new())),
            active_elections: Arc::new(AecService::new_null()),
            vote_cache: Arc::new(VoteCache::new_null()),
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
