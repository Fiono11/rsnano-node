use std::sync::{Arc, RwLock};

use tracing::debug;

use rsnano_ledger::{BlockError, LedgerEvent, ProcessResult};
use rsnano_types::{Block, QualifiedRoot};
use rsnano_utils::EventHandlerMut;

use super::{AecService, ForkCache};
use crate::{block_processing::LedgerPipelineEvent, consensus::vote_cache::VoteCache};

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
                // Publish to the cache before checking the AEC. An election starting
                // concurrently must find the fork either here or in its startup scan.
                self.fork_cache.write().unwrap().add(result.block.clone());
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
        // Opt-in evidence for epoch-membership recovery experiments.
        static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TRACE.get_or_init(|| std::env::var_os("RAI_FORK_TRACE").is_some()) {
            eprintln!(
                "FORK_TRACE {}",
                serde_json::json!({
                    "pid":std::process::id(),"root":fork.qualified_root(),"hash":fork.hash(),"added":added
                })
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::AecInsertRequest;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{SavedBlock, StateBlockArgs};

    #[test]
    fn fork_is_cached_before_an_election_can_start_after_delivery() {
        let inserter = AecForkInserter::new_test_instance();
        let args = StateBlockArgs::new_test_instance();
        let block = SavedBlock::new_test_instance_with(args.clone().into());
        let fork: Block = StateBlockArgs {
            representative: 999.into(),
            ..args
        }
        .into();
        // The ledger event processor can pause after this plugin's delivery and
        // before its remaining event handlers. An election may start in that gap.
        inserter.handle_forks(&[ProcessResult {
            block: fork.clone(),
            status: Err(BlockError::Fork),
            source: rsnano_ledger::BlockSource::Live,
            saved_block: None,
            priority: Default::default(),
        }]);
        inserter
            .active_elections
            .insert(
                AecInsertRequest::new_manual(block.clone(), Default::default()),
                Timestamp::new_test_instance(),
            )
            .unwrap();
        inserter.try_add_cached_forks(&block.qualified_root());
        assert!(
            inserter
                .active_elections
                .election_for_block(&fork.hash())
                .is_some()
        );
        assert!(
            inserter
                .fork_cache
                .read()
                .unwrap()
                .contains(&block.qualified_root())
        );
    }
}
