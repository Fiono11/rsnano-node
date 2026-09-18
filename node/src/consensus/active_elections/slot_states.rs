use rsnano_types::{Account, ConsensusEpoch};
use rustc_hash::FxHashMap;

use crate::consensus::election::{EpochSlot, LocalSlotState};

/// Kudzu: what this node voted per slot (account height) and epoch. Every
/// epoch is its own Kudzu instance, so the one-shot first vote is per epoch.
/// All epochs of a slot are dropped together once the height is finalized.
#[derive(Default)]
pub(crate) struct SlotStates {
    by_slot: FxHashMap<(Account, u64), Vec<(ConsensusEpoch, LocalSlotState)>>,
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

    /// Drop the states of all epochs of this slot
    pub fn remove_slot(&mut self, account: Account, height: u64) {
        if let Some(states) = self.by_slot.remove(&(account, height)) {
            self.len -= states.len();
        }
    }

    /// Drop the state of one epoch of a slot
    pub fn remove(&mut self, slot: &EpochSlot) {
        let Some(states) = self.by_slot.get_mut(&(slot.account, slot.height)) else {
            return;
        };
        if let Some(position) = states.iter().position(|(epoch, _)| *epoch == slot.epoch) {
            states.remove(position);
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
}
