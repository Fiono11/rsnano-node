use std::collections::{BTreeMap, HashMap};

use rsnano_types::{BlockHash, ConsensusEpoch, PublicKey};

use crate::consensus::election::{CertifiedBlock, ResidualKind};

/// RAI, "Reports that remain reconstructible": the votes this node received,
/// by epoch and voter. A reporter's residual object is its own votes that
/// its certified inventory does not summarize; a validator that holds those
/// votes derives the object itself, against the inventory it reconstructed,
/// and needs nothing fetched. The votes are kept until the epoch is long
/// decided; the signatures were checked when the votes arrived.
#[derive(Default)]
pub(crate) struct VoteRecords {
    by_epoch: BTreeMap<
        ConsensusEpoch,
        HashMap<PublicKey, BTreeMap<(CertifiedBlock, ResidualKind), BlockHash>>,
    >,
    len: usize,
}

impl VoteRecords {
    /// Records one vote of a voter for a block of an epoch, with the parent
    /// the block names. A vote seen twice is one vote.
    pub fn record(
        &mut self,
        epoch: ConsensusEpoch,
        voter: PublicKey,
        block: CertifiedBlock,
        kind: ResidualKind,
        previous: BlockHash,
    ) {
        let votes = self
            .by_epoch
            .entry(epoch)
            .or_default()
            .entry(voter)
            .or_default();
        if votes.insert((block, kind), previous).is_none() {
            self.len += 1;
        }
    }

    /// The votes of one voter in one epoch, in canonical order
    pub fn votes_of(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
    ) -> Vec<(CertifiedBlock, ResidualKind, BlockHash)> {
        self.by_epoch
            .get(&epoch)
            .and_then(|voters| voters.get(voter))
            .map(|votes| {
                votes
                    .iter()
                    .map(|((block, kind), previous)| (*block, *kind, *previous))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drops the epochs before the given one
    pub fn trim_before(&mut self, epoch: ConsensusEpoch) {
        while let Some(oldest) = self.by_epoch.keys().next().copied() {
            if oldest >= epoch {
                break;
            }
            if let Some(voters) = self.by_epoch.remove(&oldest) {
                self.len -= voters.values().map(BTreeMap::len).sum::<usize>();
            }
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::Account;

    #[test]
    fn records_votes_by_epoch_and_voter_once() {
        let mut records = VoteRecords::default();
        let block = CertifiedBlock::new(Account::from(1), 2, BlockHash::from(3));
        let voter = PublicKey::from(7);
        records.record(
            ConsensusEpoch::ZERO,
            voter,
            block,
            ResidualKind::First,
            BlockHash::from(4),
        );
        records.record(
            ConsensusEpoch::ZERO,
            voter,
            block,
            ResidualKind::First,
            BlockHash::from(4),
        );
        records.record(
            ConsensusEpoch::ZERO,
            voter,
            block,
            ResidualKind::Final,
            BlockHash::from(4),
        );
        records.record(
            ConsensusEpoch::new(1),
            voter,
            block,
            ResidualKind::First,
            BlockHash::from(4),
        );
        assert_eq!(records.len(), 3);
        assert_eq!(
            records.votes_of(ConsensusEpoch::ZERO, &voter),
            vec![
                (block, ResidualKind::First, BlockHash::from(4)),
                (block, ResidualKind::Final, BlockHash::from(4)),
            ]
        );
        assert!(
            records
                .votes_of(ConsensusEpoch::ZERO, &PublicKey::from(8))
                .is_empty()
        );

        records.trim_before(ConsensusEpoch::new(1));
        assert_eq!(records.len(), 1);
        assert!(records.votes_of(ConsensusEpoch::ZERO, &voter).is_empty());
        assert_eq!(records.votes_of(ConsensusEpoch::new(1), &voter).len(), 1);
    }
}
