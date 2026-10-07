use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use rsnano_types::{BlockHash, ConsensusEpoch, PublicKey, Vote, VoteKind};

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
    /// RAI: who voted for each block in each epoch, by kind, and the signed
    /// votes themselves. A report's certified entries are checked against
    /// the certificates these assemble, and a node lacking them is sent the
    /// original signed votes.
    support: BTreeMap<ConsensusEpoch, HashMap<BlockHash, HashSupport>>,
}

/// RAI: the identities whose signed votes of one kind this node holds for
/// one block in one epoch, and the votes
#[derive(Clone, Debug, Default)]
pub(crate) struct HashSupport {
    pub first: BTreeSet<PublicKey>,
    pub final_: BTreeSet<PublicKey>,
    votes: Vec<Arc<Vote>>,
}

impl HashSupport {
    /// The signed votes for the block, one per voter and kind
    pub fn votes(&self) -> &[Arc<Vote>] {
        &self.votes
    }
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

    /// RAI: a signed account vote received, indexed by every block it names,
    /// whether or not the block is placed here yet
    pub fn support_vote(&mut self, vote: &Arc<Vote>, hashes: impl IntoIterator<Item = BlockHash>) {
        let kind = vote.kind();
        if !matches!(kind, VoteKind::First | VoteKind::Final) {
            return;
        }
        let epoch = self.support.entry(vote.epoch).or_default();
        for hash in hashes {
            let support = epoch.entry(hash).or_default();
            let voters = if kind == VoteKind::First {
                &mut support.first
            } else {
                &mut support.final_
            };
            if voters.insert(vote.voter) {
                support.votes.push(vote.clone());
            }
        }
    }

    /// RAI: who voted for a block in an epoch
    pub fn support(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> Option<&HashSupport> {
        self.support.get(&epoch)?.get(hash)
    }

    /// RAI: whether this node holds a voter's signed vote of a kind for a
    /// block in an epoch
    pub fn has_vote(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
        hash: &BlockHash,
        kind: ResidualKind,
    ) -> bool {
        self.support(epoch, hash).is_some_and(|support| match kind {
            ResidualKind::First => support.first.contains(voter),
            ResidualKind::Final => support.final_.contains(voter),
        })
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
        self.support.retain(|held, _| *held >= epoch);
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

    #[test]
    fn signed_votes_are_indexed_by_block_once_per_voter_and_kind() {
        use rsnano_types::PrivateKey;
        let mut records = VoteRecords::default();
        let key = PrivateKey::from(1);
        let (a, b) = (BlockHash::from(1), BlockHash::from(2));
        let first = Arc::new(Vote::new_in_epoch(
            &key,
            VoteKind::First,
            ConsensusEpoch::ZERO,
            vec![a, b],
        ));
        let again = Arc::new(Vote::new_in_epoch(
            &key,
            VoteKind::First,
            ConsensusEpoch::ZERO,
            vec![a],
        ));
        let final_ = Arc::new(Vote::new_in_epoch(
            &key,
            VoteKind::Final,
            ConsensusEpoch::ZERO,
            vec![a],
        ));
        records.support_vote(&first, [a, b]);
        records.support_vote(&again, [a]);
        records.support_vote(&final_, [a]);
        let support = records.support(ConsensusEpoch::ZERO, &a).unwrap();
        assert!(support.first.contains(&key.public_key()));
        assert!(support.final_.contains(&key.public_key()));
        assert_eq!(support.votes().len(), 2);
        assert!(records.has_vote(
            ConsensusEpoch::ZERO,
            &key.public_key(),
            &b,
            ResidualKind::First
        ));
        assert!(!records.has_vote(
            ConsensusEpoch::ZERO,
            &key.public_key(),
            &b,
            ResidualKind::Final
        ));
        records.trim_before(ConsensusEpoch::new(1));
        assert!(records.support(ConsensusEpoch::ZERO, &a).is_none());
    }
}
