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
    pub fn disconnect(&mut self, hash: &BlockHash) {
        #[cfg(not(feature = "rai_protocol"))]
        self.elections.remove(hash);
        #[cfg(feature = "rai_protocol")]
        self.elections.retain(|(candidate, _), _| candidate != hash);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn disconnect_for_epoch(&mut self, hash: &BlockHash, epoch: u64) {
        self.elections.remove(&(*hash, epoch));
    }

    pub fn qualified_root(&self, hash: &BlockHash) -> Option<&QualifiedRoot> {
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.elections.get(hash)
        }
        #[cfg(feature = "rai_protocol")]
        {
            self.elections
                .iter()
                .find_map(|((candidate, _), root)| (candidate == hash).then_some(root))
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn qualified_root_for_epoch(&self, hash: &BlockHash, epoch: u64) -> Option<&QualifiedRoot> {
        self.elections.get(&(*hash, epoch))
    }

    #[cfg(feature = "rai_protocol")]
    pub fn qualified_roots(&self, hash: &BlockHash) -> Vec<QualifiedRoot> {
        self.elections
            .iter()
            .filter_map(|((candidate, _), root)| (candidate == hash).then_some(root.clone()))
            .collect()
    }

    pub fn is_active(&self, hash: &BlockHash) -> bool {
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.elections.contains_key(hash)
        }
        #[cfg(feature = "rai_protocol")]
        {
            self.elections
                .keys()
                .any(|(candidate, _)| candidate == hash)
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        [(
            "elections",
            self.elections.len(),
            size_of::<BlockHash>() + size_of::<u64>() + size_of::<QualifiedRoot>(),
        )]
        .into()
    }
}
