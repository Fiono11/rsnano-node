use std::collections::BTreeMap;

use rsnano_types::{BlockHash, ConsensusEpoch, Root};

use crate::consensus::{
    ReportRecord, SlotRecord,
    election::{ResidualKind, VoteType},
};

/// RAI, durable signing records: votes a restarted node recorded, of one
/// epoch and type. The records hold what the node decided to sign, written
/// before the signature exists, and a report lists those votes; the
/// signatures themselves are gone with the process. Other validators need
/// them to verify the report, and a vote every receiver dropped can come
/// from nobody else, so the node signs these statements again.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RecordedVotes {
    pub epoch: ConsensusEpoch,
    pub vote_type: VoteType,
    pub roots: Vec<Root>,
    pub hashes: Vec<BlockHash>,
}

/// The first and final votes in the restored slot records and frozen
/// reports, one statement each, in batches of at most `max_hashes`. A report
/// can list a decision whose slot record the crash lost, so it is read too.
pub(crate) fn recorded_votes(
    slots: &[SlotRecord],
    reports: &[ReportRecord],
    max_hashes: usize,
) -> Vec<RecordedVotes> {
    let mut votes: BTreeMap<(ConsensusEpoch, ResidualKind), BTreeMap<BlockHash, Root>> =
        BTreeMap::new();
    let mut add = |epoch, kind, hash, root_of: &dyn Fn() -> Root| {
        votes
            .entry((epoch, kind))
            .or_default()
            .entry(hash)
            .or_insert_with(root_of);
    };
    for record in slots {
        let root = || {
            let parent = |hash: BlockHash| {
                record
                    .parents
                    .iter()
                    .find(|(held, _)| *held == hash)
                    .map_or(BlockHash::ZERO, |(_, parent)| *parent)
            };
            move |hash| root_of(parent(hash), record.slot.account.into())
        };
        let epoch = record.slot.epoch;
        if let Some(hash) = record.state.first_voted {
            let kind = if record.state.first_early {
                ResidualKind::EarlyFirst
            } else {
                ResidualKind::First
            };
            add(epoch, kind, hash, &|| root()(hash));
        }
        if let Some(hash) = record.state.final_voted {
            add(epoch, ResidualKind::Final, hash, &|| root()(hash));
        }
    }
    for record in reports {
        for (block, kind, previous) in record.residual.entries() {
            add(record.epoch, kind, block.hash, &|| {
                root_of(previous, block.account.into())
            });
        }
    }
    let mut batches = Vec::new();
    for ((epoch, kind), blocks) in votes {
        let blocks: Vec<(BlockHash, Root)> = blocks.into_iter().collect();
        for chunk in blocks.chunks(max_hashes) {
            batches.push(RecordedVotes {
                epoch,
                vote_type: vote_type_of(kind),
                roots: chunk.iter().map(|(_, root)| *root).collect(),
                hashes: chunk.iter().map(|(hash, _)| *hash).collect(),
            });
        }
    }
    batches
}

/// A block's root: its parent, or its account for the first block
fn root_of(parent: BlockHash, account: Root) -> Root {
    if parent.is_zero() {
        account
    } else {
        parent.into()
    }
}

fn vote_type_of(kind: ResidualKind) -> VoteType {
    match kind {
        ResidualKind::First => VoteType::NonFinal,
        ResidualKind::EarlyFirst => VoteType::EarlyFirst,
        ResidualKind::Final => VoteType::Final,
    }
}

#[cfg(test)]
mod tests {
    use rsnano_types::Account;

    use super::*;
    use crate::consensus::election::{
        CertifiedBlock, CertifiedState, EpochSlot, LocalSlotState, ResidualVotes,
    };

    #[test]
    fn slot_records_give_their_first_and_final_votes() {
        let (settled, early, last) = (hash(1), hash(2), hash(3));
        let batches = recorded_votes(
            &[
                slot(1, 7, settled, false, Some(settled), hash(50)),
                slot(1, 8, early, true, None, settled),
                slot(2, 9, last, false, None, BlockHash::ZERO),
            ],
            &[],
            255,
        );
        assert_eq!(
            batches,
            vec![
                batch(1, VoteType::NonFinal, vec![(settled, hash(50).into())]),
                batch(1, VoteType::EarlyFirst, vec![(early, settled.into())]),
                batch(1, VoteType::Final, vec![(settled, hash(50).into())]),
                batch(2, VoteType::NonFinal, vec![(last, account().into())]),
            ]
        );
    }

    #[test]
    fn a_vote_only_the_frozen_report_lists_is_included_once() {
        let (both, only_report) = (hash(1), hash(2));
        let batches = recorded_votes(
            &[slot(1, 7, both, false, None, hash(50))],
            &[report(1, &[(both, hash(50)), (only_report, hash(60))])],
            255,
        );
        assert_eq!(
            batches,
            vec![batch(
                1,
                VoteType::NonFinal,
                vec![(both, hash(50).into()), (only_report, hash(60).into())]
            )]
        );
    }

    #[test]
    fn batches_respect_the_vote_size() {
        let slots: Vec<SlotRecord> = (1..=5)
            .map(|i| slot(1, i, hash(i), false, None, hash(50)))
            .collect();
        let sizes: Vec<usize> = recorded_votes(&slots, &[], 2)
            .iter()
            .map(|batch| batch.hashes.len())
            .collect();
        assert_eq!(sizes, vec![2, 2, 1]);
    }

    /* Test helpers */

    fn hash(value: u64) -> BlockHash {
        BlockHash::from(value)
    }

    fn account() -> Account {
        Account::from(100)
    }

    fn slot(
        epoch: u64,
        height: u64,
        first: BlockHash,
        early: bool,
        final_voted: Option<BlockHash>,
        parent: BlockHash,
    ) -> SlotRecord {
        SlotRecord {
            slot: EpochSlot {
                account: account(),
                height,
                epoch: ConsensusEpoch::new(epoch),
            },
            state: LocalSlotState {
                first_voted: Some(first),
                first_early: early,
                final_voted,
                ..Default::default()
            },
            parents: vec![(first, parent)],
        }
    }

    fn report(epoch: u64, votes: &[(BlockHash, BlockHash)]) -> ReportRecord {
        let mut residual = ResidualVotes::default();
        for (i, (block, parent)) in votes.iter().enumerate() {
            residual.record(
                CertifiedBlock::new(account(), 7 + i as u64, *block),
                *parent,
                ResidualKind::First,
            );
        }
        ReportRecord {
            epoch: ConsensusEpoch::new(epoch),
            signed: Vec::new(),
            certified: CertifiedState::new(),
            residual,
            certified_changes: None,
        }
    }

    fn batch(epoch: u64, vote_type: VoteType, votes: Vec<(BlockHash, Root)>) -> RecordedVotes {
        RecordedVotes {
            epoch: ConsensusEpoch::new(epoch),
            vote_type,
            roots: votes.iter().map(|(_, root)| *root).collect(),
            hashes: votes.iter().map(|(hash, _)| *hash).collect(),
        }
    }
}
