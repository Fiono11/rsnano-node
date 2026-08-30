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
}

impl VoteRouter {
    /// Add a route for 'hash' to an election by its qualified root
    /// Existing routes will be replaced
    pub fn connect(&mut self, hash: BlockHash, root: QualifiedRoot) {
        #[cfg(feature = "rai_protocol")]
        let epoch = root.epoch;
        #[cfg(not(feature = "rai_protocol"))]
        let epoch = 0;
        self.elections.insert((hash, epoch), root);
    }

    /// Remove all routes to this election
    pub fn disconnect_election(&mut self, election: &Election) {
        for hash in election.candidate_blocks().keys() {
            #[cfg(feature = "rai_protocol")]
            let epoch = election.qualified_root().epoch;
            #[cfg(not(feature = "rai_protocol"))]
            let epoch = 0;
            self.elections.remove(&(*hash, epoch));
        }
    }

    /// Remove route to this block
    pub fn disconnect(&mut self, hash: &BlockHash) {
        self.elections
            .retain(|(routed_hash, _), _| routed_hash != hash);
    }

    #[cfg(feature = "rai_protocol")]
    pub fn disconnect_epoch(&mut self, hash: &BlockHash, epoch: u64) {
        self.elections.remove(&(*hash, epoch));
    }

    pub fn qualified_root(&self, hash: &BlockHash, epoch: u64) -> Option<&QualifiedRoot> {
        self.elections
            .get(&(*hash, epoch))
            .or_else(|| self.elections.get(&(*hash, 0)))
    }

    pub fn qualified_root_any_epoch(&self, hash: &BlockHash) -> Option<&QualifiedRoot> {
        self.elections
            .iter()
            .filter(|((routed_hash, _), _)| routed_hash == hash)
            .max_by_key(|((_, epoch), _)| *epoch)
            .map(|(_, root)| root)
    }

    pub fn is_active(&self, hash: &BlockHash) -> bool {
        self.elections
            .keys()
            .any(|(routed_hash, _)| routed_hash == hash)
    }

    pub fn container_info(&self) -> ContainerInfo {
        [(
            "elections",
            self.elections.len(),
            size_of::<(BlockHash, u64)>() + size_of::<QualifiedRoot>(),
        )]
        .into()
    }
}
