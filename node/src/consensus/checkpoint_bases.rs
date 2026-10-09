use std::{collections::BTreeMap, sync::RwLock};

use rsnano_types::{BlockHash, ConsensusEpoch};

use crate::consensus::election::SettledBase;

/// RAI, "Every first vote names its base": the state hashes `d` of the
/// decided checkpoints this node installed, and of the closed genesis state.
/// The active elections record each one as it is installed; the vote
/// generators read the base a first vote names from here, and the active
/// elections which first votes count as settled. A first vote of epoch `e`
/// is cast on the predecessor checkpoint `S_{e-1}` once that is installed
/// (a settled vote) and on the last closed one before it, `S_{e-2}`,
/// before (an early vote); before epoch 0 stands the genesis state.
#[derive(Default)]
pub struct CheckpointBases(RwLock<Bases>);

#[derive(Default)]
struct Bases {
    genesis: Option<BlockHash>,
    decided: BTreeMap<ConsensusEpoch, BlockHash>,
}

impl CheckpointBases {
    pub fn set_genesis(&self, state: BlockHash) {
        self.0.write().unwrap().genesis = Some(state);
    }

    pub fn insert(&self, epoch: ConsensusEpoch, state: BlockHash) {
        self.0.write().unwrap().decided.insert(epoch, state);
    }

    /// The state hash of the checkpoint `back` epochs before `epoch`; the
    /// genesis state stands before epoch 0
    pub fn before(&self, epoch: ConsensusEpoch, back: u64) -> Option<BlockHash> {
        let bases = self.0.read().unwrap();
        match epoch.as_u64().checked_sub(back) {
            Some(before) => bases.decided.get(&ConsensusEpoch::new(before)).copied(),
            None => bases.genesis,
        }
    }

    /// The base a first vote of an epoch names: the predecessor checkpoint
    /// for a settled vote, the closed one before it for an early vote, zero
    /// if this node does not hold it
    pub fn first_vote_base(&self, epoch: ConsensusEpoch, early: bool) -> BlockHash {
        self.before(epoch, if early { 2 } else { 1 })
            .unwrap_or(BlockHash::ZERO)
    }

    /// Which first votes of an epoch count as settled here: those naming
    /// its predecessor checkpoint, once this node installed it
    pub fn settled_base(&self, epoch: ConsensusEpoch) -> SettledBase {
        match self.before(epoch, 1) {
            Some(state) => SettledBase::Known(state),
            None => SettledBase::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settled_vote_names_the_predecessor_and_an_early_vote_the_one_before() {
        let bases = CheckpointBases::default();
        bases.set_genesis(BlockHash::from(100));
        bases.insert(ConsensusEpoch::ZERO, BlockHash::from(1));

        assert_eq!(
            bases.first_vote_base(ConsensusEpoch::ZERO, false),
            BlockHash::from(100)
        );
        assert_eq!(
            bases.first_vote_base(ConsensusEpoch::new(1), false),
            BlockHash::from(1)
        );
        assert_eq!(
            bases.first_vote_base(ConsensusEpoch::new(1), true),
            BlockHash::from(100)
        );
        assert_eq!(
            bases.first_vote_base(ConsensusEpoch::new(2), true),
            BlockHash::from(1)
        );
        // Not installed here: no base
        assert_eq!(
            bases.first_vote_base(ConsensusEpoch::new(2), false),
            BlockHash::ZERO
        );
        assert_eq!(
            bases.settled_base(ConsensusEpoch::new(2)),
            SettledBase::Unknown
        );
        assert_eq!(
            bases.settled_base(ConsensusEpoch::new(1)),
            SettledBase::Known(BlockHash::from(1))
        );
    }
}
