use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use rsnano_types::{BlockHash, ConsensusEpoch, PublicKey, Vote, VoteKind};

use crate::consensus::election::{AccountSlot, CertifiedBlock, ResidualKind};

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
    /// RAI, late notarization: notarization-only votes cast for the block
    /// after their signers left the epoch. They count towards an exclusion
    /// witness of the epoch and towards nothing else: no NC, FF or FC. They
    /// are retained evidence, served by hash like the other votes, and a
    /// manifest may carry them inside exclusion witnesses.
    pub late: BTreeSet<PublicKey>,
    /// RAI, "no fast path on early votes": the first voters whose first vote
    /// was settled, cast after installing the epoch's predecessor
    /// checkpoint; only these count towards a fast certificate
    pub settled: BTreeSet<PublicKey>,
    /// Where the block sits, once a vote for it was placed here: what tells
    /// whether the support can still discharge a lock record at a position
    slot: Option<AccountSlot>,
    votes: Vec<Arc<Vote>>,
}

impl HashSupport {
    /// The signed votes for the block, one per voter and kind
    pub fn votes(&self) -> &[Arc<Vote>] {
        &self.votes
    }

    /// Every member supporting the block in the epoch: first votes, late
    /// notarizations, and final votes, whose correct signers first voted it
    pub fn supporters(&self) -> BTreeSet<&PublicKey> {
        self.first
            .iter()
            .chain(self.final_.iter())
            .chain(self.late.iter())
            .collect()
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
    /// whether or not the block is placed here yet. A notarization-only
    /// account vote is a late notarization.
    pub fn support_vote(&mut self, vote: &Arc<Vote>, hashes: impl IntoIterator<Item = BlockHash>) {
        let kind = vote.kind();
        if !matches!(kind, VoteKind::First | VoteKind::Final | VoteKind::Notar) {
            return;
        }
        let epoch = self.support.entry(vote.epoch).or_default();
        for hash in hashes {
            let support = epoch.entry(hash).or_default();
            let voters = match kind {
                VoteKind::First => &mut support.first,
                VoteKind::Final => &mut support.final_,
                _ => &mut support.late,
            };
            let new = voters.insert(vote.voter);
            // A settled signature of a first vote held as early is evidence
            // of its own: it is what a fast certificate is assembled from
            let settled =
                kind == VoteKind::First && !vote.is_early() && support.settled.insert(vote.voter);
            if new || settled {
                support.votes.push(vote.clone());
            }
        }
    }

    /// RAI: where a block with support in an epoch sits
    pub fn place(&mut self, epoch: ConsensusEpoch, hash: &BlockHash, slot: AccountSlot) {
        if let Some(support) = self
            .support
            .get_mut(&epoch)
            .and_then(|support| support.get_mut(hash))
        {
            support.slot.get_or_insert(slot);
        }
    }

    /// RAI, late notarization: who cast a notarization-only vote for a block
    /// in an epoch
    pub fn late_notarizers(
        &self,
        epoch: ConsensusEpoch,
        hash: &BlockHash,
    ) -> Option<&BTreeSet<PublicKey>> {
        self.support(epoch, hash)
            .map(|support| &support.late)
            .filter(|late| !late.is_empty())
    }

    /// RAI, late notarization: the blocks with notarization-only votes in an
    /// epoch, and their voters
    pub fn late_notarized_blocks(
        &self,
        epoch: ConsensusEpoch,
    ) -> impl Iterator<Item = (&BlockHash, &BTreeSet<PublicKey>)> {
        self.support
            .get(&epoch)
            .into_iter()
            .flatten()
            .filter(|(_, support)| !support.late.is_empty())
            .map(|(hash, support)| (hash, &support.late))
    }

    /// RAI: whether this node holds a voter's settled first vote for a block
    /// in an epoch
    pub fn has_settled_vote(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
        hash: &BlockHash,
    ) -> bool {
        self.support(epoch, hash)
            .is_some_and(|support| support.settled.contains(voter))
    }

    /// RAI: whether this node holds a voter's late notarization for a block
    /// in an epoch
    pub fn has_late_vote(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
        hash: &BlockHash,
    ) -> bool {
        self.support(epoch, hash)
            .is_some_and(|support| support.late.contains(voter))
    }

    /// RAI: who voted for a block in an epoch
    pub fn support(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> Option<&HashSupport> {
        self.support.get(&epoch)?.get(hash)
    }

    /// RAI: the support held for a block in an epoch and every earlier one
    pub fn support_until<'a>(
        &'a self,
        epoch: ConsensusEpoch,
        hash: &'a BlockHash,
    ) -> impl Iterator<Item = &'a HashSupport> + 'a {
        self.support
            .range(..=epoch)
            .filter_map(move |(_, support)| support.get(hash))
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

    /// Drops the epochs before the given one. The support of an older epoch
    /// survives where `discharges` says it may still discharge a lock record
    /// of that origin at the block's position: an exclusion witness is kept
    /// as long as the records it can discharge.
    pub fn trim_before(
        &mut self,
        epoch: ConsensusEpoch,
        discharges: impl Fn(ConsensusEpoch, &AccountSlot) -> bool,
    ) {
        while let Some(oldest) = self.by_epoch.keys().next().copied() {
            if oldest >= epoch {
                break;
            }
            if let Some(voters) = self.by_epoch.remove(&oldest) {
                self.len -= voters.values().map(BTreeMap::len).sum::<usize>();
            }
        }
        for (held, support) in self.support.iter_mut() {
            if *held < epoch {
                support
                    .retain(|_, support| support.slot.is_some_and(|slot| discharges(*held, &slot)));
            }
        }
        self.support.retain(|_, support| !support.is_empty());
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

        records.trim_before(ConsensusEpoch::new(1), |_, _| false);
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
        records.trim_before(ConsensusEpoch::new(1), |_, _| false);
        assert!(records.support(ConsensusEpoch::ZERO, &a).is_none());
    }

    /// Late notarizations are support of their own kind, served with the
    /// other votes, and an older epoch's support outlives the trim where it
    /// may still discharge a lock record at its block's position. A
    /// notarization-only kind exists under `rai_protocol` only.
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn late_notarizations_are_kept_while_they_may_discharge_a_record() {
        use rsnano_types::PrivateKey;
        let mut records = VoteRecords::default();
        let key = PrivateKey::from(1);
        let (a, b) = (BlockHash::from(1), BlockHash::from(2));
        let late = Arc::new(Vote::new_in_epoch(
            &key,
            VoteKind::Notar,
            ConsensusEpoch::ZERO,
            vec![a, b],
        ));
        records.support_vote(&late, [a, b]);
        let support = records.support(ConsensusEpoch::ZERO, &a).unwrap();
        assert!(support.first.is_empty() && support.final_.is_empty());
        assert_eq!(support.supporters().len(), 1);
        assert_eq!(support.votes().len(), 1);
        assert!(records.has_late_vote(ConsensusEpoch::ZERO, &key.public_key(), &a));
        assert_eq!(
            records.late_notarized_blocks(ConsensusEpoch::ZERO).count(),
            2
        );

        let locked = AccountSlot::new(rsnano_types::Account::from(5), 3);
        records.place(ConsensusEpoch::ZERO, &a, locked);
        records.place(
            ConsensusEpoch::ZERO,
            &b,
            AccountSlot::new(rsnano_types::Account::from(6), 3),
        );
        records.trim_before(ConsensusEpoch::new(5), |origin, slot| {
            origin == ConsensusEpoch::ZERO && *slot == locked
        });
        assert!(records.has_late_vote(ConsensusEpoch::ZERO, &key.public_key(), &a));
        assert!(records.support(ConsensusEpoch::ZERO, &b).is_none());
        records.trim_before(ConsensusEpoch::new(5), |_, _| false);
        assert!(records.support(ConsensusEpoch::ZERO, &a).is_none());
    }
}
