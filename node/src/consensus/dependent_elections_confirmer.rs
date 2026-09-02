use std::sync::Arc;

use super::{AecService, election::ConfirmedElection};
use crate::cementation::ConfirmingSet;
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{BlockHash, SavedBlock};

pub(crate) struct DependentElectionsConfirmer {
    pub(crate) confirming_set: Arc<ConfirmingSet>,
    pub(crate) active_elections: Arc<AecService>,
    pub(crate) clock: Arc<SteadyClock>,
}

impl DependentElectionsConfirmer {
    pub fn new_null() -> Self {
        Self {
            confirming_set: Arc::new(ConfirmingSet::new_null()),
            active_elections: Arc::new(AecService::new_null()),
            clock: Arc::new(SteadyClock::new_null()),
        }
    }

    /// Confirmed blocks might implicitly confirm dependent elections
    pub fn confirm_dependent_elections(
        &self,
        confirmed_blocks: &Vec<(SavedBlock, BlockHash, u64)>,
    ) {
        let blocks_plus_election = self.blocks_plus_elections(confirmed_blocks);
        let now = self.clock.now();

        self.active_elections
            .confirm_dependent_elections(blocks_plus_election, now);
    }

    fn blocks_plus_elections(
        &self,
        blocks: &Vec<(SavedBlock, BlockHash, u64)>,
    ) -> Vec<(SavedBlock, Option<ConfirmedElection>, u64)> {
        let mut blocks_with_election = Vec::with_capacity(blocks.len());

        self.confirming_set.do_election_cache(|cache| {
            for (confirmed_block, confirmation_root, confirmation_epoch) in blocks {
                // Every block in this cementation batch carries the hash of the election winner
                // which initiated confirmation. Dependencies must inherit that winner's epoch.
                let source_election = cache.get(confirmation_root).cloned();
                blocks_with_election.push((
                    confirmed_block.clone(),
                    source_election,
                    *confirmation_epoch,
                ));
            }
        });

        blocks_with_election
    }
}
