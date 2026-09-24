use std::sync::Arc;

use rsnano_types::BlockHash;
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::AecService;
use crate::consensus::vote_cache::VoteCache;

/// Skip passive phase for blocks without cached votes to avoid bootstrap delays
pub(crate) struct BootstrapElectionActivator {
    pub active_elections: Arc<AecService>,
    pub vote_cache: Arc<VoteCache>,
    pub stats: Arc<Stats>,
}
impl BootstrapElectionActivator {
    /// The elections started for blocks without cached votes skip their
    /// passive phase. Done for a batch under one AEC lock: taken for each
    /// start, the lock held up the AEC fact thread behind the AEC's writers.
    pub(crate) fn elections_started(&self, hashes: &[BlockHash]) {
        // A block with cached votes is probably not a bootstrap election
        let uncached: Vec<BlockHash> = hashes
            .iter()
            .filter(|hash| !self.vote_cache.contains(hash))
            .copied()
            .collect();
        if uncached.is_empty() {
            return;
        }

        let activated = self.active_elections.transition_active_batch(&uncached);

        self.stats.add(
            StatType::ActiveElections,
            DetailType::ActivateImmediately,
            activated as u64,
        );
    }
}
