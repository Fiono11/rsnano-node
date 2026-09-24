use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use rsnano_types::{BlockHash, ConsensusEpoch, PublicKey, Vote, VoteKind};

use crate::consensus::election::{CertifiedBlock, ResidualKind};

/// RAI: the identities whose signed votes of one kind this node retains for
/// one block in one epoch. Each identity counts once per kind; a certificate
/// is assembled from these against the committee of the epoch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HashSupport {
    pub first: BTreeSet<PublicKey>,
    pub notar: BTreeSet<PublicKey>,
    pub final_: BTreeSet<PublicKey>,
}

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
    signed: BTreeMap<
        ConsensusEpoch,
        HashMap<PublicKey, BTreeMap<(BlockHash, ResidualKind), Arc<Vote>>>,
    >,
    /// The identities behind every retained signed vote, by epoch and block:
    /// what a tagged N or F entry of a report is checked against
    support: BTreeMap<ConsensusEpoch, HashMap<BlockHash, HashSupport>>,
}

impl VoteRecords {
    /// Called only after ingress signature validation. Keep the original batch,
    /// including its signature: splitting or re-signing would change the proof.
    /// Placement may not yet be known when the signed vote arrives.
    pub fn retain_signed(&mut self, vote: &Arc<Vote>) {
        let kind = match vote.kind() {
            VoteKind::First => ResidualKind::First,
            VoteKind::Notar => ResidualKind::Notar,
            VoteKind::Final => ResidualKind::Final,
            _ => return,
        };
        if vote.epoch.is_close_round() {
            return;
        }
        let held = self
            .signed
            .entry(vote.epoch)
            .or_default()
            .entry(vote.voter)
            .or_default();
        let support = self.support.entry(vote.epoch).or_default();
        for hash in &vote.hashes {
            held.entry((*hash, kind)).or_insert_with(|| vote.clone());
            let identities = support.entry(*hash).or_default();
            match kind {
                ResidualKind::First => identities.first.insert(vote.voter),
                ResidualKind::Notar => identities.notar.insert(vote.voter),
                ResidualKind::Final => identities.final_.insert(vote.voter),
            };
        }
    }

    /// The identities whose signed votes this node retains for a block in
    /// an epoch, if any
    /// Every hash of an epoch with the identities that voted for it
    pub fn supports(
        &self,
        epoch: ConsensusEpoch,
    ) -> impl Iterator<Item = (&BlockHash, &HashSupport)> + '_ {
        self.support.get(&epoch).into_iter().flatten()
    }

    pub fn support(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> Option<&HashSupport> {
        self.support.get(&epoch)?.get(hash)
    }

    /// Every retained signed batch of any voter covering one of the hashes
    /// in the epoch: what is relayed to a replica that can not justify a
    /// tagged entry from the votes it holds
    pub fn signed_for_hashes(&self, epoch: ConsensusEpoch, hashes: &[BlockHash]) -> Vec<Arc<Vote>> {
        let Some(voters) = self.signed.get(&epoch) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for held in voters.values() {
            for hash in hashes {
                for kind in [
                    ResidualKind::First,
                    ResidualKind::Notar,
                    ResidualKind::Final,
                ] {
                    if let Some(vote) = held.get(&(*hash, kind))
                        && seen.insert(Arc::as_ptr(vote) as usize)
                    {
                        result.push(vote.clone());
                    }
                }
            }
        }
        result
    }

    pub fn signed_for(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
        hashes: &[BlockHash],
    ) -> Vec<Arc<Vote>> {
        let Some(held) = self.signed.get(&epoch).and_then(|e| e.get(voter)) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for hash in hashes {
            for kind in [
                ResidualKind::First,
                ResidualKind::Notar,
                ResidualKind::Final,
            ] {
                if let Some(vote) = held.get(&(*hash, kind)) {
                    if seen.insert(Arc::as_ptr(vote) as usize) {
                        result.push(vote.clone());
                    }
                }
            }
        }
        result
    }

    pub fn unplaced_signed(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
    ) -> Vec<(BlockHash, ResidualKind)> {
        let placed: HashSet<_> = self
            .votes_of(epoch, voter)
            .into_iter()
            .map(|(b, k, _)| (b.hash, k))
            .collect();
        self.signed
            .get(&epoch)
            .and_then(|e| e.get(voter))
            .into_iter()
            .flat_map(|votes| votes.keys())
            .filter(|key| !placed.contains(key))
            .copied()
            .collect()
    }

    /// Placement comes from owner-signed block data, never a report/sketch hint.
    /// A matching original signature must already have passed vote ingress.
    pub fn place_signed(
        &mut self,
        epoch: ConsensusEpoch,
        voter: PublicKey,
        block: CertifiedBlock,
        kind: ResidualKind,
        previous: BlockHash,
    ) {
        if self
            .signed
            .get(&epoch)
            .and_then(|e| e.get(&voter))
            .is_some_and(|v| v.contains_key(&(block.hash, kind)))
        {
            self.record(epoch, voter, block, kind, previous);
        }
    }

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

    /// How many votes of one voter in one epoch are held here, placed or
    /// only signed: what changes when a new vote arrives
    pub fn count_of(&self, epoch: ConsensusEpoch, voter: &PublicKey) -> usize {
        let placed = self
            .by_epoch
            .get(&epoch)
            .and_then(|voters| voters.get(voter))
            .map_or(0, BTreeMap::len);
        let signed = self
            .signed
            .get(&epoch)
            .and_then(|voters| voters.get(voter))
            .map_or(0, BTreeMap::len);
        placed + signed
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
        self.signed.retain(|held, _| *held >= epoch);
        self.support.retain(|held, _| *held >= epoch);
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
    fn placement_requires_a_matching_retained_signature() {
        let mut records = VoteRecords::default();
        let key = rsnano_types::PrivateKey::from(7);
        let b = CertifiedBlock::new(Account::from(1), 1, BlockHash::from(2));
        let epoch = ConsensusEpoch::ZERO;
        records.place_signed(
            epoch,
            key.public_key(),
            b,
            ResidualKind::First,
            BlockHash::ZERO,
        );
        assert!(records.votes_of(epoch, &key.public_key()).is_empty());
        let vote = Arc::new(Vote::new_in_epoch(
            &key,
            VoteKind::First,
            epoch,
            vec![b.hash],
        ));
        records.retain_signed(&vote);
        assert_eq!(
            records.unplaced_signed(epoch, &key.public_key()),
            vec![(b.hash, ResidualKind::First)]
        );
        records.place_signed(
            epoch,
            key.public_key(),
            b,
            ResidualKind::Final,
            BlockHash::ZERO,
        );
        assert!(records.votes_of(epoch, &key.public_key()).is_empty());
        records.place_signed(
            epoch,
            key.public_key(),
            b,
            ResidualKind::First,
            BlockHash::ZERO,
        );
        assert_eq!(
            records.votes_of(epoch, &key.public_key()),
            vec![(b, ResidualKind::First, BlockHash::ZERO)]
        );
        assert!(records.unplaced_signed(epoch, &key.public_key()).is_empty());
    }

    #[test]
    fn signed_replay_preserves_batches_and_is_scoped_and_trimmed() {
        let key = rsnano_types::PrivateKey::from(7);
        let hashes = vec![BlockHash::from(1), BlockHash::from(2)];
        let first = Arc::new(Vote::new_in_epoch(
            &key,
            VoteKind::First,
            ConsensusEpoch::ZERO,
            hashes.clone(),
        ));
        let final_vote = Arc::new(Vote::new_in_epoch(
            &key,
            VoteKind::Final,
            ConsensusEpoch::ZERO,
            hashes.clone(),
        ));
        let mut records = VoteRecords::default();
        records.retain_signed(&first);
        records.retain_signed(&first);
        records.retain_signed(&final_vote);
        let replay = records.signed_for(ConsensusEpoch::ZERO, &key.public_key(), &hashes);
        assert_eq!(replay.len(), 2);
        assert!(Arc::ptr_eq(&replay[0], &first));
        assert!(Arc::ptr_eq(&replay[1], &final_vote));
        assert!(replay.iter().all(|v| v.validate().is_ok()));
        assert_eq!(replay[0].hashes, hashes);
        assert!(
            records
                .signed_for(ConsensusEpoch::new(1), &key.public_key(), &hashes)
                .is_empty()
        );
        assert!(
            records
                .signed_for(ConsensusEpoch::ZERO, &PublicKey::from(8), &hashes)
                .is_empty()
        );
        records.trim_before(ConsensusEpoch::new(1));
        assert!(
            records
                .signed_for(ConsensusEpoch::ZERO, &key.public_key(), &hashes)
                .is_empty()
        );
    }

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
