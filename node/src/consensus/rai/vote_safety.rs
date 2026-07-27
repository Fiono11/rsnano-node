use std::collections::{BTreeMap, BTreeSet};

use rsnano_types::{
    BlockHash, PublicKey, RaiElectionId, RaiElectionValue, RaiEpoch, RaiSlot, RaiVote,
};

use super::{RaiActiveElectionsSnapshot, RaiCloseState, RaiVoteStateSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiVoteSafetyError {
    ConflictingUnreleasedSlotVote,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RaiVoteSafetySnapshot {
    pub entries: Vec<RaiVoteSafetyEntrySnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiVoteSafetyEntrySnapshot {
    pub voter: PublicKey,
    pub slot: RaiSlot,
    pub epoch: RaiEpoch,
    pub blocks: Vec<BlockHash>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RaiVoteSafetyKey {
    voter: PublicKey,
    slot: RaiSlot,
    epoch: RaiEpoch,
}

#[derive(Default)]
pub struct RaiVoteSafety {
    votes: BTreeMap<RaiVoteSafetyKey, BTreeSet<BlockHash>>,
}

impl RaiVoteSafety {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_snapshot(snapshot: RaiVoteSafetySnapshot) -> Self {
        let mut safety = Self::new();
        for entry in snapshot.entries {
            let key = RaiVoteSafetyKey {
                voter: entry.voter,
                slot: entry.slot,
                epoch: entry.epoch,
            };
            safety.votes.entry(key).or_default().extend(entry.blocks);
        }

        safety
    }

    pub fn snapshot(&self) -> RaiVoteSafetySnapshot {
        RaiVoteSafetySnapshot {
            entries: self
                .votes
                .iter()
                .map(|(key, blocks)| RaiVoteSafetyEntrySnapshot {
                    voter: key.voter,
                    slot: key.slot,
                    epoch: key.epoch,
                    blocks: blocks.iter().copied().collect(),
                })
                .collect(),
        }
    }

    pub fn merge_active_elections(&mut self, snapshot: &RaiActiveElectionsSnapshot) {
        for election in &snapshot.elections {
            let RaiElectionId::Slot { slot, epoch } = &election.id else {
                continue;
            };

            for vote_state in &election.vote_states {
                for block in block_votes(vote_state) {
                    self.record_block(vote_state.voter, *slot, *epoch, block);
                }
            }
        }
    }

    pub fn record_vote(&mut self, vote: &RaiVote) {
        let Some((slot, epoch, block)) = slot_block_vote(vote) else {
            return;
        };

        self.record_block(vote.voter, slot, epoch, block);
    }

    pub fn snapshot_entry_for_vote(&self, vote: &RaiVote) -> Option<RaiVoteSafetyEntrySnapshot> {
        let (slot, epoch, _) = slot_block_vote(vote)?;
        let key = RaiVoteSafetyKey {
            voter: vote.voter,
            slot,
            epoch,
        };
        let blocks = self.votes.get(&key)?;
        Some(RaiVoteSafetyEntrySnapshot {
            voter: key.voter,
            slot: key.slot,
            epoch: key.epoch,
            blocks: blocks.iter().copied().collect(),
        })
    }

    pub fn validate(
        &self,
        close_state: &RaiCloseState,
        vote: &RaiVote,
    ) -> Result<(), RaiVoteSafetyError> {
        let Some((slot, epoch, block)) = slot_block_vote(vote) else {
            return Ok(());
        };

        for (key, blocks) in &self.votes {
            if key.voter != vote.voter || key.slot != slot || key.epoch == epoch {
                continue;
            }

            let earlier_epoch = std::cmp::min(epoch, key.epoch);
            if close_state.is_slot_vote_released(earlier_epoch, &slot) {
                continue;
            }

            if blocks.iter().any(|existing_block| *existing_block != block) {
                return Err(RaiVoteSafetyError::ConflictingUnreleasedSlotVote);
            }
        }

        Ok(())
    }

    fn record_block(&mut self, voter: PublicKey, slot: RaiSlot, epoch: RaiEpoch, block: BlockHash) {
        self.votes
            .entry(RaiVoteSafetyKey { voter, slot, epoch })
            .or_default()
            .insert(block);
    }
}

fn slot_block_vote(vote: &RaiVote) -> Option<(RaiSlot, RaiEpoch, BlockHash)> {
    let RaiElectionId::Slot { slot, epoch } = &vote.election_id else {
        return None;
    };
    let RaiElectionValue::Block(hash) = &vote.value else {
        return None;
    };

    Some((*slot, *epoch, *hash))
}

fn block_votes(vote_state: &RaiVoteStateSnapshot) -> impl Iterator<Item = BlockHash> + '_ {
    vote_state
        .first
        .iter()
        .chain(vote_state.notarized.iter())
        .chain(vote_state.final_vote.iter())
        .filter_map(|value| match value {
            RaiElectionValue::Block(hash) => Some(*hash),
            RaiElectionValue::CloseCutHash(_)
            | RaiElectionValue::CloseRecordHash(_)
            | RaiElectionValue::Timeout => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::rai::{
        RaiClosedSlotState, RaiElectionSnapshot, RaiElectionStatus, RaiTallySnapshot, VisibleSlots,
    };
    use rsnano_types::{Account, PrivateKey};

    #[test]
    fn conflicting_later_same_slot_vote_is_unsafe_until_certified_release() {
        let key = PrivateKey::from(1);
        let slot = slot(1);
        let first_block = BlockHash::from(3);
        let retry_block = BlockHash::from(4);
        let mut safety = RaiVoteSafety::new();
        safety.record_vote(&slot_vote(&key, slot, 0, first_block));

        assert_eq!(
            safety.validate(
                &RaiCloseState::new(),
                &slot_vote(&key, slot, 1, retry_block)
            ),
            Err(RaiVoteSafetyError::ConflictingUnreleasedSlotVote)
        );
        assert_eq!(
            safety.validate(
                &RaiCloseState::new(),
                &slot_vote(&key, slot, 1, first_block)
            ),
            Ok(())
        );

        let mut close_state = RaiCloseState::new();
        close_state.start_closing(0).unwrap();
        close_state.install_cut(0, VisibleSlots::new()).unwrap();
        close_state
            .record_cut_drain(0, std::iter::empty::<(RaiSlot, RaiClosedSlotState)>())
            .unwrap();
        close_state.record_current_close_record_value(0).unwrap();
        close_state.advance_epoch(0).unwrap();

        assert_eq!(
            safety.validate(&close_state, &slot_vote(&key, slot, 1, retry_block)),
            Ok(())
        );
    }

    #[test]
    fn safety_history_is_scoped_by_voter_and_slot() {
        let key = PrivateKey::from(1);
        let other_key = PrivateKey::from(2);
        let first_slot = slot(1);
        let other_slot = slot(2);
        let mut safety = RaiVoteSafety::new();
        safety.record_vote(&slot_vote(&key, first_slot, 0, BlockHash::from(3)));

        assert_eq!(
            safety.validate(
                &RaiCloseState::new(),
                &slot_vote(&other_key, first_slot, 1, BlockHash::from(4))
            ),
            Ok(())
        );
        assert_eq!(
            safety.validate(
                &RaiCloseState::new(),
                &slot_vote(&key, other_slot, 1, BlockHash::from(4))
            ),
            Ok(())
        );
    }

    #[test]
    fn snapshot_roundtrip_canonicalizes_duplicate_history() {
        let key = PrivateKey::from(1);
        let snapshot = RaiVoteSafetySnapshot {
            entries: vec![
                RaiVoteSafetyEntrySnapshot {
                    voter: key.public_key(),
                    slot: slot(1),
                    epoch: 0,
                    blocks: vec![BlockHash::from(5), BlockHash::from(4), BlockHash::from(5)],
                },
                RaiVoteSafetyEntrySnapshot {
                    voter: key.public_key(),
                    slot: slot(1),
                    epoch: 0,
                    blocks: vec![BlockHash::from(4)],
                },
            ],
        };

        assert_eq!(
            RaiVoteSafety::from_snapshot(snapshot).snapshot(),
            RaiVoteSafetySnapshot {
                entries: vec![RaiVoteSafetyEntrySnapshot {
                    voter: key.public_key(),
                    slot: slot(1),
                    epoch: 0,
                    blocks: vec![BlockHash::from(4), BlockHash::from(5)],
                }],
            }
        );
    }

    #[test]
    fn active_election_merge_imports_all_non_timeout_slot_block_votes() {
        let key = PrivateKey::from(1);
        let slot = slot(1);
        let mut safety = RaiVoteSafety::new();
        safety.merge_active_elections(&RaiActiveElectionsSnapshot {
            elections: vec![RaiElectionSnapshot {
                id: RaiElectionId::Slot { slot, epoch: 0 },
                status: RaiElectionStatus::Active,
                vote_states: vec![RaiVoteStateSnapshot {
                    voter: key.public_key(),
                    committee_index: 0,
                    first: Some(RaiElectionValue::Block(BlockHash::from(3))),
                    notarized: vec![
                        RaiElectionValue::Block(BlockHash::from(4)),
                        RaiElectionValue::Timeout,
                    ],
                    final_vote: Some(RaiElectionValue::Block(BlockHash::from(5))),
                }],
                tallies: vec![RaiTallySnapshot {
                    value: RaiElectionValue::Timeout,
                    per_committee: vec![1],
                }],
                notarization_tallies: Vec::new(),
                final_tallies: Vec::new(),
                winner: None,
                confirmed_value: None,
            }],
        });

        assert_eq!(
            safety.snapshot(),
            RaiVoteSafetySnapshot {
                entries: vec![RaiVoteSafetyEntrySnapshot {
                    voter: key.public_key(),
                    slot,
                    epoch: 0,
                    blocks: vec![BlockHash::from(3), BlockHash::from(4), BlockHash::from(5)],
                }],
            }
        );
        assert_eq!(
            safety.validate(
                &RaiCloseState::new(),
                &slot_vote(&key, slot, 1, BlockHash::from(6))
            ),
            Err(RaiVoteSafetyError::ConflictingUnreleasedSlotVote)
        );
    }

    fn slot(account: u64) -> RaiSlot {
        RaiSlot::new(Account::from(account), 1)
    }

    fn slot_vote(key: &PrivateKey, slot: RaiSlot, epoch: RaiEpoch, block: BlockHash) -> RaiVote {
        RaiVote::new_first(
            key,
            RaiElectionId::Slot { slot, epoch },
            RaiElectionValue::Block(block),
        )
    }
}
