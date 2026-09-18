use std::{collections::HashMap, mem::size_of};

use rsnano_types::{BlockHash, ConsensusEpoch, QualifiedRoot};
use rsnano_utils::container_info::ContainerInfo;

use crate::consensus::election::{Election, ElectionId};

/// The elections a block is a candidate in: one root, one election per epoch
struct Route {
    root: QualifiedRoot,
    /// Ascending, usually a single epoch
    epochs: Vec<ConsensusEpoch>,
}

/// This class routes votes to their associated election
#[derive(Default)]
pub(crate) struct VoteRouter {
    // Mapping of block hashes to elections.
    // Election already contains the associated block
    elections: HashMap<BlockHash, Route>,
}

impl VoteRouter {
    /// Add a route for 'hash' to an election
    pub fn connect(&mut self, hash: BlockHash, id: ElectionId) {
        let route = self.elections.entry(hash).or_insert_with(|| Route {
            root: id.root.clone(),
            epochs: Vec::with_capacity(1),
        });
        debug_assert_eq!(route.root, id.root);
        if let Err(i) = route.epochs.binary_search(&id.epoch) {
            route.epochs.insert(i, id.epoch);
        }
    }

    /// Remove all routes to this election
    pub fn disconnect_election(&mut self, election: &Election) {
        for hash in election.candidate_blocks().keys() {
            self.disconnect(hash, election.epoch());
        }
    }

    /// Remove the route from this block to the election of this epoch
    pub fn disconnect(&mut self, hash: &BlockHash, epoch: ConsensusEpoch) {
        let Some(route) = self.elections.get_mut(hash) else {
            return;
        };
        if let Ok(i) = route.epochs.binary_search(&epoch) {
            route.epochs.remove(i);
        }
        if route.epochs.is_empty() {
            self.elections.remove(hash);
        }
    }

    /// The election of the given epoch this block is a candidate in
    pub fn election_id(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> Option<ElectionId> {
        let route = self.elections.get(hash)?;
        route
            .epochs
            .binary_search(&epoch)
            .ok()
            .map(|_| ElectionId::new(route.root.clone(), epoch))
    }

    /// The elections this block is a candidate in, oldest epoch first
    pub fn elections_of(&self, hash: &BlockHash) -> impl Iterator<Item = ElectionId> + '_ {
        self.elections.get(hash).into_iter().flat_map(|route| {
            route
                .epochs
                .iter()
                .map(move |epoch| ElectionId::new(route.root.clone(), *epoch))
        })
    }

    /// The election of the newest epoch this block is a candidate in
    pub fn latest_election_id(&self, hash: &BlockHash) -> Option<ElectionId> {
        let route = self.elections.get(hash)?;
        let epoch = *route.epochs.last()?;
        Some(ElectionId::new(route.root.clone(), epoch))
    }

    pub fn container_info(&self) -> ContainerInfo {
        [(
            "elections",
            self.elections.len(),
            size_of::<BlockHash>() + size_of::<Route>(),
        )]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_per_epoch() {
        let mut router = VoteRouter::default();
        let hash = BlockHash::from(1);
        let root = QualifiedRoot::new_test_instance();
        let epoch0 = ElectionId::new(root.clone(), ConsensusEpoch::ZERO);
        let epoch1 = ElectionId::new(root.clone(), ConsensusEpoch::new(1));
        router.connect(hash, epoch1.clone());
        router.connect(hash, epoch0.clone());

        assert_eq!(
            router.election_id(&hash, ConsensusEpoch::ZERO),
            Some(epoch0)
        );
        assert_eq!(
            router.election_id(&hash, ConsensusEpoch::new(1)),
            Some(epoch1.clone())
        );
        assert_eq!(router.election_id(&hash, ConsensusEpoch::new(2)), None);
        assert_eq!(router.latest_election_id(&hash), Some(epoch1));

        router.disconnect(&hash, ConsensusEpoch::new(1));
        assert_eq!(router.election_id(&hash, ConsensusEpoch::new(1)), None);
        assert!(router.latest_election_id(&hash).is_some());
        router.disconnect(&hash, ConsensusEpoch::ZERO);
        assert_eq!(router.latest_election_id(&hash), None);
    }
}
