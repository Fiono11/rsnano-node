use std::{collections::HashMap, mem::size_of};

use rsnano_types::{BlockHash, QualifiedRoot};
use rsnano_utils::container_info::ContainerInfo;

use crate::consensus::election::Election;

/// This class routes votes to their associated election
#[derive(Default)]
pub(crate) struct VoteRouter {
    // Mapping of block hashes to elections.
    // Election already contains the associated block
    #[cfg(not(feature = "rai_protocol"))]
    elections: HashMap<BlockHash, QualifiedRoot>,
    #[cfg(feature = "rai_protocol")]
    elections: HashMap<(BlockHash, u64), QualifiedRoot>,
}

impl VoteRouter {
    /// Add a route for 'hash' to an election by its qualified root
    /// Existing routes will be replaced
    pub fn connect(&mut self, hash: BlockHash, root: QualifiedRoot) {
        #[cfg(not(feature = "rai_protocol"))]
        self.elections.insert(hash, root);
        #[cfg(feature = "rai_protocol")]
        self.elections.insert((hash, root.epoch), root);
    }

    /// Remove all routes to this election
    pub fn disconnect_election(&mut self, election: &Election) {
        for hash in election.candidate_blocks().keys() {
            #[cfg(not(feature = "rai_protocol"))]
            self.elections.remove(hash);
            #[cfg(feature = "rai_protocol")]
            self.elections
                .remove(&(*hash, election.qualified_root().epoch));
        }
    }

    /// Remove route to this block
    pub fn disconnect(&mut self, hash: &BlockHash, #[cfg(feature = "rai_protocol")] epoch: u64) {
        #[cfg(not(feature = "rai_protocol"))]
        self.elections.remove(hash);
        #[cfg(feature = "rai_protocol")]
        self.elections.remove(&(*hash, epoch));
    }

    pub fn qualified_root(
        &self,
        hash: &BlockHash,
        #[cfg(feature = "rai_protocol")] epoch: u64,
    ) -> Option<&QualifiedRoot> {
        #[cfg(not(feature = "rai_protocol"))]
        return self.elections.get(hash);
        #[cfg(feature = "rai_protocol")]
        return self.elections.get(&(*hash, epoch));
    }

    #[cfg(feature = "rai_protocol")]
    pub fn qualified_root_any_epoch(&self, hash: &BlockHash) -> Option<&QualifiedRoot> {
        self.elections
            .iter()
            .find_map(|((candidate, _), root)| (candidate == hash).then_some(root))
    }

    pub fn is_active(&self, hash: &BlockHash) -> bool {
        #[cfg(not(feature = "rai_protocol"))]
        return self.elections.contains_key(hash);
        #[cfg(feature = "rai_protocol")]
        return self
            .elections
            .keys()
            .any(|(candidate, _)| candidate == hash);
    }

    pub fn container_info(&self) -> ContainerInfo {
        [(
            "elections",
            self.elections.len(),
            size_of::<BlockHash>()
                + size_of::<QualifiedRoot>()
                + if cfg!(feature = "rai_protocol") {
                    size_of::<u64>()
                } else {
                    0
                },
        )]
        .into()
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;

    #[test]
    fn routes_same_block_independently_by_epoch() {
        let hash = BlockHash::from(1);
        let epoch_one = QualifiedRoot::new_test_instance().with_epoch(1);
        let epoch_two = epoch_one.slot().with_epoch(2);
        let mut router = VoteRouter::default();

        router.connect(hash, epoch_one.clone());
        router.connect(hash, epoch_two.clone());

        assert_eq!(router.qualified_root(&hash, 1), Some(&epoch_one));
        assert_eq!(router.qualified_root(&hash, 2), Some(&epoch_two));
        router.disconnect(&hash, 1);
        assert_eq!(router.qualified_root(&hash, 1), None);
        assert_eq!(router.qualified_root(&hash, 2), Some(&epoch_two));
    }
}
