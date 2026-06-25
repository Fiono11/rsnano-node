use std::sync::Arc;

use rsnano_types::BlockHash;
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{AecService, VoteCache};

/// Skip passive phase for blocks without cached votes to avoid bootstrap delays
pub(crate) struct BootstrapElectionActivator {
    pub active_elections: Arc<AecService>,
    pub vote_cache: Arc<VoteCache>,
    pub stats: Arc<Stats>,
}
impl BootstrapElectionActivator {
    pub(crate) fn election_started(&self, hash: BlockHash) {
        let in_cache = self.vote_cache.contains(&hash);
        if in_cache {
            // Probably not a bootstrap election
            return;
        }

        let activated = self.active_elections.transition_active(&hash);

        if activated {
            self.stats
                .inc(StatType::ActiveElections, DetailType::ActivateImmediately);
        }
    }
}
