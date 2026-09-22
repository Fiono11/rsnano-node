use rsnano_types::{Account, BlockHash, ConsensusEpoch};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::consensus::election::{EpochSlot, LocalSlotState};

/// Kudzu: what this node voted per slot (account height) and epoch. Every
/// epoch is its own Kudzu instance, so the one-shot first vote is per epoch.
/// All epochs of a slot are dropped together once the height is finalized.
#[derive(Default)]
pub(crate) struct SlotStates {
    by_slot: FxHashMap<(Account, u64), Vec<(ConsensusEpoch, LocalSlotState)>>,
    /// RAI: the parent each block this node voted for names. A residual vote
    /// in an epoch's report places its block by the branch the block
    /// continues, and a slot state is keyed by (account, height) alone: two
    /// conflicting parents put their children in one voting domain but on
    /// different branches, so the parent belongs to the block, not the slot.
    parents: FxHashMap<BlockHash, BlockHash>,
    len: usize,
}

impl SlotStates {
    pub fn get(&self, slot: &EpochSlot) -> Option<&LocalSlotState> {
        self.by_slot
            .get(&(slot.account, slot.height))?
            .iter()
            .find(|(epoch, _)| *epoch == slot.epoch)
            .map(|(_, state)| state)
    }

    pub fn get_or_default(&mut self, slot: &EpochSlot) -> &mut LocalSlotState {
        let states = self.by_slot.entry((slot.account, slot.height)).or_default();
        let position = match states.iter().position(|(epoch, _)| *epoch == slot.epoch) {
            Some(position) => position,
            None => {
                states.push((slot.epoch, LocalSlotState::default()));
                self.len += 1;
                states.len() - 1
            }
        };
        &mut states[position].1
    }

    /// RAI: the parent a block this node voted for names
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn parent(&self, hash: &BlockHash) -> BlockHash {
        self.parents.get(hash).copied().unwrap_or(BlockHash::ZERO)
    }

    /// RAI: remember which branch a block this node voted for continues
    pub fn record_parent(&mut self, hash: BlockHash, previous: BlockHash) {
        self.parents.entry(hash).or_insert(previous);
    }

    /// RAI, Section 6.1: what this node voted in every slot of one epoch:
    /// the account, the height and the slot's state. The record the epoch's
    /// report is built from.
    #[allow(dead_code)] // the RAI epoch decision uses these
    pub fn iter_epoch(
        &self,
        epoch: ConsensusEpoch,
    ) -> impl Iterator<Item = (Account, u64, &LocalSlotState)> {
        self.by_slot
            .iter()
            .flat_map(move |((account, height), states)| {
                states
                    .iter()
                    .filter(move |(slot_epoch, _)| *slot_epoch == epoch)
                    .map(move |(_, state)| (*account, *height, state))
            })
    }

    /// The blocks the states of a slot voted for, whose parents go with them
    fn voted_at(&self, account: Account, height: u64) -> Vec<BlockHash> {
        self.by_slot
            .get(&(account, height))
            .into_iter()
            .flat_map(|states| states.iter())
            .flat_map(|(_, state)| {
                state
                    .first_voted
                    .into_iter()
                    .chain(state.final_voted)
                    .chain(state.timeout_voted)
                    .chain(state.notar_voted.iter().copied())
            })
            .collect()
    }

    /// Drop the states of all epochs of this slot
    pub fn remove_slot(&mut self, account: Account, height: u64) {
        for hash in self.voted_at(account, height) {
            self.parents.remove(&hash);
        }
        if let Some(states) = self.by_slot.remove(&(account, height)) {
            self.len -= states.len();
        }
    }

    /// RAI: drop the states of one epoch but for the slots named: those
    /// with an instance still in the AEC, which votes with them
    pub fn remove_epoch_except(&mut self, epoch: ConsensusEpoch, keep: &FxHashSet<(Account, u64)>) {
        let parents = &mut self.parents;
        self.by_slot.retain(|slot, states| {
            if keep.contains(slot) {
                return true;
            }
            let before = states.len();
            states.retain(|(e, held)| {
                if *e == epoch {
                    for hash in held
                        .first_voted
                        .into_iter()
                        .chain(held.final_voted)
                        .chain(held.timeout_voted)
                        .chain(held.notar_voted.iter().copied())
                    {
                        parents.remove(&hash);
                    }
                }
                *e != epoch
            });
            self.len -= before - states.len();
            !states.is_empty()
        });
    }

    /// Drop the state of one epoch of a slot
    pub fn remove(&mut self, slot: &EpochSlot) {
        let Some(states) = self.by_slot.get_mut(&(slot.account, slot.height)) else {
            return;
        };
        if let Some(position) = states.iter().position(|(epoch, _)| *epoch == slot.epoch) {
            let (_, held) = states.remove(position);
            for hash in held
                .first_voted
                .into_iter()
                .chain(held.final_voted)
                .chain(held.timeout_voted)
                .chain(held.notar_voted)
            {
                self.parents.remove(&hash);
            }
            self.len -= 1;
        }
        if states.is_empty() {
            self.by_slot.remove(&(slot.account, slot.height));
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::BlockHash;
    use rsnano_types::VoteKind;

    #[test]
    fn states_are_per_epoch_and_dropped_per_slot() {
        let mut states = SlotStates::default();
        let slot = EpochSlot {
            account: Account::from(1),
            height: 2,
            epoch: ConsensusEpoch::ZERO,
        };
        let next_epoch = EpochSlot {
            epoch: ConsensusEpoch::new(1),
            ..slot
        };
        assert!(states.get(&slot).is_none());

        states
            .get_or_default(&slot)
            .mark_voted(BlockHash::from(3), VoteKind::First);
        assert_eq!(
            states.get(&slot).unwrap().first_voted,
            Some(BlockHash::from(3))
        );
        assert!(states.get(&next_epoch).is_none());
        states.get_or_default(&next_epoch);
        assert_eq!(states.len(), 2);

        states.remove(&next_epoch);
        assert_eq!(states.len(), 1);
        assert!(states.get(&next_epoch).is_none());
        states.remove_slot(slot.account, slot.height);
        assert_eq!(states.len(), 0);
        assert!(states.get(&slot).is_none());
    }

    /// RAI: the states of an agreed epoch go, but for the slots with an
    /// instance still in the AEC
    #[test]
    fn states_of_an_epoch_are_dropped_but_for_the_live_slots() {
        let mut states = SlotStates::default();
        let slot = |account: u64, epoch: u64| EpochSlot {
            account: Account::from(account),
            height: 1,
            epoch: ConsensusEpoch::new(epoch),
        };
        states.get_or_default(&slot(1, 0));
        states.get_or_default(&slot(1, 1));
        states.get_or_default(&slot(2, 0));
        states.get_or_default(&slot(3, 0));
        assert_eq!(states.len(), 4);

        let live = FxHashSet::from_iter([(Account::from(3), 1)]);
        states.remove_epoch_except(ConsensusEpoch::ZERO, &live);

        assert_eq!(states.len(), 2);
        assert!(states.get(&slot(1, 0)).is_none());
        assert!(states.get(&slot(2, 0)).is_none());
        assert!(states.get(&slot(3, 0)).is_some());
        assert!(states.get(&slot(1, 1)).is_some());

        states.remove_epoch_except(ConsensusEpoch::ZERO, &FxHashSet::default());
        assert_eq!(states.len(), 1);
        assert!(states.get(&slot(3, 0)).is_none());
    }
}
