use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use rsnano_types::{
    Account, Amount, BlockHash, ConsensusEpoch, PublicKey, QualifiedRoot, VoteKind,
};

use crate::consensus::election::{FinalStateHash, LocalSlotState};

/// RAI: an instance this node finalized. The election has left the AEC, but
/// the node's own statements in it stay available: a replica that missed
/// them still has to collect the certificates of the instance's epoch.
pub(crate) struct FinalizedInstance {
    pub root: QualifiedRoot,
    pub account: Account,
    pub height: u64,
    pub epoch: ConsensusEpoch,
    pub winner: BlockHash,
    pub candidates: Vec<BlockHash>,
    /// The representative and balance of the winner: what the committee
    /// derived from the epoch counts. None for a legacy block.
    pub delegation: Option<Delegation>,
    pub slot: LocalSlotState,
}

/// RAI: what a block delegates: its balance to its representative
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delegation {
    pub hash: BlockHash,
    pub representative: PublicKey,
    pub balance: Amount,
}

/// RAI: a block finalized in an instance of an epoch, with what it delegates
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedBlock {
    pub account: Account,
    pub height: u64,
    pub delegation: Delegation,
}

impl FinalizedInstance {
    /// The node's own statements, re-signed on request. All of them: a final
    /// vote notarizes as well, but only the first vote counts in
    /// allVotes(firstVote), which the settled predicate of a replica that
    /// missed it depends on.
    pub fn statements(&self) -> Vec<(VoteKind, Vec<BlockHash>)> {
        self.slot.statements_for(&self.candidates, self.winner)
    }
}

/// RAI: what each epoch finalized explicitly, i.e. by a finalization
/// certificate of an election of that epoch, and the node's own statements
/// in those instances. Finalized elections leave the AEC, so this is the
/// epoch's record of them.
#[derive(Default)]
pub(crate) struct EpochStates {
    /// The hash of the winner of every instance finalized in the epoch. The
    /// winner only: a losing candidate's notarization certificate races the
    /// finalization certificate, and the instance is erased the moment it
    /// finalizes, so whether the loser was notarized by then differs from
    /// replica to replica. The value the close agrees on must not depend on
    /// that; the loser is not what the epoch decided anyway.
    by_epoch: BTreeMap<ConsensusEpoch, FinalStateHash>,
    /// The finalized instances per epoch
    count_by_epoch: BTreeMap<ConsensusEpoch, u64>,
    /// The finalized instances each candidate block took part in, by epoch.
    /// A block decided while the epoch changed can be finalized in both.
    instances: HashMap<BlockHash, Vec<Arc<FinalizedInstance>>>,
    len: usize,
}

impl EpochStates {
    pub fn record_finalized(&mut self, instance: FinalizedInstance) {
        if self.instance(&instance.winner, instance.epoch).is_some() {
            return;
        }
        let hash = self.by_epoch.entry(instance.epoch).or_default();
        hash.add(&instance.account, instance.height, &instance.winner);
        *self.count_by_epoch.entry(instance.epoch).or_default() += 1;
        let instance = Arc::new(instance);
        for candidate in &instance.candidates {
            self.instances
                .entry(*candidate)
                .or_default()
                .push(instance.clone());
        }
        self.len += 1;
    }

    /// Any finalized instance this block was a candidate in, whatever the
    /// epoch: what places the block
    pub fn any_instance(&self, hash: &BlockHash) -> Option<&FinalizedInstance> {
        self.instances
            .get(hash)?
            .first()
            .map(|instance| instance.as_ref())
    }

    /// The finalized instance of the given epoch this block was a candidate in
    pub fn instance(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> Option<&FinalizedInstance> {
        self.instances
            .get(hash)?
            .iter()
            .find(|i| i.epoch == epoch)
            .map(|i| i.as_ref())
    }

    /// Whether this block was finalized explicitly in the given epoch
    pub fn finalized_in_epoch(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> bool {
        self.instance(hash, epoch)
            .is_some_and(|i| i.winner == *hash)
    }

    /// Whether this node cast its own final vote for the block in the epoch
    /// it was finalized in; the statement it can hand out again
    pub fn final_voted_in_epoch(&self, hash: &BlockHash, epoch: ConsensusEpoch) -> bool {
        self.instance(hash, epoch)
            .is_some_and(|i| i.winner == *hash && i.slot.final_voted == Some(*hash))
    }

    /// Whether this block was finalized explicitly in any epoch
    pub fn is_finalized(&self, hash: &BlockHash) -> bool {
        self.instances
            .get(hash)
            .is_some_and(|instances| instances.iter().any(|i| i.winner == *hash))
    }

    /// The explicitly finalized state per epoch
    pub fn by_epoch(&self) -> &BTreeMap<ConsensusEpoch, FinalStateHash> {
        &self.by_epoch
    }

    /// The instances finalized in the epoch
    pub fn finalized_count(&self, epoch: ConsensusEpoch) -> u64 {
        self.count_by_epoch.get(&epoch).copied().unwrap_or(0)
    }

    /// The winner of every instance finalized in the given epoch, with what
    /// it delegates: what the epoch's state hashes for these instances
    pub fn finalized_blocks_in(&self, epoch: ConsensusEpoch) -> Vec<FinalizedBlock> {
        let mut seen: Vec<FinalizedBlock> = Vec::new();
        for instances in self.instances.values() {
            for instance in instances.iter().filter(|i| i.epoch == epoch) {
                let Some(delegation) = instance.delegation else {
                    continue;
                };
                let entry = FinalizedBlock {
                    account: instance.account,
                    height: instance.height,
                    delegation,
                };
                if !seen.contains(&entry) {
                    seen.push(entry);
                }
            }
        }
        seen
    }

    /// The blocks finalized in the given epoch, as (account, height, hash)
    pub fn finalized_in(&self, epoch: ConsensusEpoch) -> Vec<(Account, u64, BlockHash)> {
        self.instances
            .iter()
            .filter_map(|(hash, instances)| {
                instances
                    .iter()
                    .find(|i| i.epoch == epoch && i.winner == *hash)
                    .map(|i| (i.account, i.height, i.winner))
            })
            .collect()
    }

    /// RAI: the instances of one epoch which finalized and left the AEC
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn instances_of(&self, epoch: ConsensusEpoch) -> impl Iterator<Item = &FinalizedInstance> {
        self.instances.iter().filter_map(move |(hash, instances)| {
            instances
                .iter()
                .find(|i| i.epoch == epoch && i.winner == *hash)
                .map(|i| i.as_ref())
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_each_instance_once_per_epoch() {
        let mut states = EpochStates::default();
        let epoch1 = ConsensusEpoch::new(1);
        let account = Account::from(1);
        let block = BlockHash::from(1);
        let fork = BlockHash::from(3);
        let mut slot = LocalSlotState::default();
        slot.mark_voted(block, VoteKind::First);
        slot.mark_voted(block, VoteKind::Final);
        states.record_finalized(instance(
            &account,
            ConsensusEpoch::ZERO,
            block,
            &[block, fork],
            slot,
        ));
        states.record_finalized(instance(
            &Account::from(2),
            epoch1,
            BlockHash::from(2),
            &[BlockHash::from(2)],
            LocalSlotState::default(),
        ));
        // A second record in the same epoch changes nothing, one in another epoch counts
        states.record_finalized(instance(
            &account,
            ConsensusEpoch::ZERO,
            block,
            &[block],
            LocalSlotState::default(),
        ));
        states.record_finalized(instance(
            &account,
            epoch1,
            block,
            &[block],
            LocalSlotState::default(),
        ));
        assert_eq!(
            states.finalized_in(ConsensusEpoch::ZERO),
            vec![(account, 1, block)]
        );
        assert_eq!(states.finalized_in(epoch1).len(), 2);

        assert_eq!(states.len(), 3);
        assert!(states.finalized_in_epoch(&block, ConsensusEpoch::ZERO));
        assert!(states.finalized_in_epoch(&block, epoch1));
        assert!(states.final_voted_in_epoch(&block, ConsensusEpoch::ZERO));
        assert!(!states.final_voted_in_epoch(&block, epoch1));
        assert!(!states.final_voted_in_epoch(&BlockHash::from(2), epoch1));
        assert!(!states.finalized_in_epoch(&BlockHash::from(2), ConsensusEpoch::ZERO));
        assert!(states.is_finalized(&BlockHash::from(2)));
        assert!(!states.is_finalized(&BlockHash::from(3)));

        // The losing candidate finds the instance and the node's statements in it
        let by_fork = states.instance(&fork, ConsensusEpoch::ZERO).unwrap();
        assert_eq!(by_fork.winner, block);
        assert!(!states.finalized_in_epoch(&fork, ConsensusEpoch::ZERO));
        assert_eq!(
            by_fork.statements(),
            vec![
                (VoteKind::First, vec![block]),
                (VoteKind::Final, vec![block])
            ]
        );
        let mut fork_voter = LocalSlotState::default();
        fork_voter.mark_voted(fork, VoteKind::First);
        let lost = instance(&account, epoch1, block, &[block, fork], fork_voter);
        assert_eq!(lost.statements(), vec![(VoteKind::First, vec![fork])]);

        let mut expected = FinalStateHash::default();
        expected.add(&account, 1, &block);
        assert_eq!(states.by_epoch()[&ConsensusEpoch::ZERO], expected);
        assert_eq!(states.by_epoch()[&epoch1].entries(), 2);
    }

    /// The value the close agrees on hashes what the epoch decided: the
    /// winner, whether or not the losing candidate got a notarization
    /// certificate before the instance was erased
    #[test]
    fn epoch_hash_covers_the_winner_only() {
        let account = Account::from(1);
        let winner = BlockHash::from(1);
        let loser = BlockHash::from(2);
        let mut with_loser = EpochStates::default();
        with_loser.record_finalized(instance(
            &account,
            ConsensusEpoch::ZERO,
            winner,
            &[winner, loser],
            LocalSlotState::default(),
        ));
        let mut winner_only = EpochStates::default();
        winner_only.record_finalized(instance(
            &account,
            ConsensusEpoch::ZERO,
            winner,
            &[winner],
            LocalSlotState::default(),
        ));
        assert_eq!(
            with_loser.by_epoch()[&ConsensusEpoch::ZERO],
            winner_only.by_epoch()[&ConsensusEpoch::ZERO]
        );
        let blocks = with_loser.finalized_blocks_in(ConsensusEpoch::ZERO);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].delegation.hash, winner);
        assert_eq!(blocks[0].height, 1);
    }

    /*
     * Test helpers
     */

    fn instance(
        account: &Account,
        epoch: ConsensusEpoch,
        winner: BlockHash,
        candidates: &[BlockHash],
        slot: LocalSlotState,
    ) -> FinalizedInstance {
        FinalizedInstance {
            root: QualifiedRoot::new_test_instance(),
            account: *account,
            height: 1,
            epoch,
            winner,
            candidates: candidates.to_vec(),
            delegation: Some(Delegation {
                hash: winner,
                representative: PublicKey::from(7),
                balance: Amount::raw(10),
            }),
            slot,
        }
    }
}
