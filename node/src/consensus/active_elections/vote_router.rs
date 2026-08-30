use std::{collections::HashMap, mem::size_of};

use rsnano_types::{BlockHash, QualifiedRoot};
use rsnano_utils::container_info::ContainerInfo;

use crate::consensus::election::Election;

/// This class routes votes to their associated election
#[derive(Default)]
pub(crate) struct VoteRouter {
    // Mapping of block hashes and protocol epochs to elections.
    // Election already contains the associated block
    elections: HashMap<(BlockHash, u64), QualifiedRoot>,
    // The newest route for each hash. Votes from an older RAI epoch may arrive
    // after the exact route has gone away; keep that fallback lookup O(1)
    // instead of scanning every active election while the AEC write lock is held.
    latest: HashMap<BlockHash, (u64, QualifiedRoot)>,
}

impl VoteRouter {
    /// Add a route for 'hash' to an election by its qualified root
    /// Existing routes will be replaced
    pub fn connect(&mut self, hash: BlockHash, root: QualifiedRoot) {
        #[cfg(feature = "rai_protocol")]
        let epoch = root.epoch;
        #[cfg(not(feature = "rai_protocol"))]
        let epoch = 0;
        self.elections.insert((hash, epoch), root.clone());
        self.latest
            .entry(hash)
            .and_modify(|current| {
                if epoch >= current.0 {
                    *current = (epoch, root.clone());
                }
            })
            .or_insert((epoch, root));
    }

    /// Remove all routes to this election
    pub fn disconnect_election(&mut self, election: &Election) {
        for hash in election.candidate_blocks().keys() {
            #[cfg(feature = "rai_protocol")]
            let epoch = election.qualified_root().epoch;
            #[cfg(not(feature = "rai_protocol"))]
            let epoch = 0;
            self.remove_route(hash, epoch);
        }
    }

    /// Remove route to this block
    pub fn disconnect(&mut self, hash: &BlockHash) {
        self.elections
            .retain(|(routed_hash, _), _| routed_hash != hash);
        self.latest.remove(hash);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn disconnect_epoch(&mut self, hash: &BlockHash, epoch: u64) {
        self.remove_route(hash, epoch);
    }

    pub fn qualified_root(&self, hash: &BlockHash, epoch: u64) -> Option<&QualifiedRoot> {
        self.elections
            .get(&(*hash, epoch))
            .or_else(|| self.elections.get(&(*hash, 0)))
    }

    pub fn qualified_root_any_epoch(&self, hash: &BlockHash) -> Option<&QualifiedRoot> {
        self.latest.get(hash).map(|(_, root)| root)
    }

    pub fn is_active(&self, hash: &BlockHash) -> bool {
        self.latest.contains_key(hash)
    }

    fn remove_route(&mut self, hash: &BlockHash, epoch: u64) {
        self.elections.remove(&(*hash, epoch));
        if self
            .latest
            .get(hash)
            .is_some_and(|current| current.0 == epoch)
        {
            let replacement = self
                .elections
                .iter()
                .filter(|((routed_hash, _), _)| routed_hash == hash)
                .max_by_key(|((_, candidate_epoch), _)| *candidate_epoch)
                .map(|((_, candidate_epoch), root)| (*candidate_epoch, root.clone()));
            if let Some(replacement) = replacement {
                self.latest.insert(*hash, replacement);
            } else {
                self.latest.remove(hash);
            }
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        [
            (
                "elections",
                self.elections.len(),
                size_of::<(BlockHash, u64)>() + size_of::<QualifiedRoot>(),
            ),
            (
                "latest",
                self.latest.len(),
                size_of::<BlockHash>() + size_of::<(u64, QualifiedRoot)>(),
            ),
        ]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_epoch_returns_latest_route() {
        let mut router = VoteRouter::default();
        let hash = BlockHash::from(1);
        let epoch_1 = QualifiedRoot::new_test_instance().with_epoch(1);
        let epoch_2 = QualifiedRoot::new_test_instance().with_epoch(2);

        router.connect(hash, epoch_1);
        router.connect(hash, epoch_2.clone());

        assert_eq!(router.qualified_root_any_epoch(&hash), Some(&epoch_2));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn removing_latest_route_restores_previous_epoch() {
        let mut router = VoteRouter::default();
        let hash = BlockHash::from(1);
        let epoch_1 = QualifiedRoot::new_test_instance().with_epoch(1);
        let epoch_2 = QualifiedRoot::new_test_instance().with_epoch(2);

        router.connect(hash, epoch_1.clone());
        router.connect(hash, epoch_2);
        router.disconnect_epoch(&hash, 2);

        assert_eq!(router.qualified_root_any_epoch(&hash), Some(&epoch_1));
        assert!(router.is_active(&hash));
        router.disconnect_epoch(&hash, 1);
        assert!(!router.is_active(&hash));
    }
}
