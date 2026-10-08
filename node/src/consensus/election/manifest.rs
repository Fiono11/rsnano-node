use std::collections::{BTreeMap, BTreeSet};

use rsnano_messages::ManifestEntry;
use rsnano_types::{Amount, Blake2HashBuilder, BlockHash, ConsensusEpoch, PublicKey};

use super::{CertificateKinds, Committee, ResidualKind};

/// RAI: the members of a committee in canonical key order; bit `i` of a
/// manifest entry names member `i`. Committees of more than 64 members
/// cannot be named this way, and no manifest is built for them.
pub struct MemberOrder(Vec<PublicKey>);

impl MemberOrder {
    pub const MAX_MEMBERS: usize = 64;

    pub fn of(committee: &Committee) -> Option<Self> {
        if committee.len() > Self::MAX_MEMBERS {
            return None;
        }
        let mut members: Vec<PublicKey> = committee.weights().keys().copied().collect();
        members.sort();
        Some(Self(members))
    }

    pub fn bit(&self, member: &PublicKey) -> Option<u64> {
        self.0
            .iter()
            .position(|held| held == member)
            .map(|i| 1u64 << i)
    }

    pub fn mask(&self, members: impl IntoIterator<Item = PublicKey>) -> u64 {
        members
            .into_iter()
            .filter_map(|member| self.bit(&member))
            .fold(0, |mask, bit| mask | bit)
    }

    pub fn members(&self, mask: u64) -> impl Iterator<Item = &PublicKey> + '_ {
        self.0
            .iter()
            .enumerate()
            .filter(move |(i, _)| mask & (1u64 << i) != 0)
            .map(|(_, member)| member)
    }

    fn weight(&self, committee: &Committee, mask: u64) -> Amount {
        self.members(mask).fold(Amount::ZERO, |sum, member| {
            sum.number()
                .checked_add(committee.weight(member).number())
                .map(Amount::raw)
                .unwrap_or(Amount::MAX)
        })
    }
}

/// RAI, "Immutable candidate inputs": the evidence manifest `M` a candidate
/// commits to by its digest `mu_e`. For every block a selected report's
/// claims rest on, in every epoch whose votes justify it, it names the
/// voters whose signed first and final votes the leader used, and the late
/// notarizers, which count only inside exclusion witnesses. A validator
/// checks a candidate against the manifest's votes, which it must hold, not
/// against whatever votes it happens to have; so the state digest is a
/// function of fixed inputs, and two validators that accept a value used
/// the same evidence.
/// The voter bit sets of one manifest entry
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Voters {
    first: u64,
    final_: u64,
    late: u64,
    settled: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    entries: BTreeMap<(ConsensusEpoch, BlockHash), Voters>,
}

impl Manifest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, entry: ManifestEntry) {
        if entry.first == 0 && entry.final_ == 0 && entry.late == 0 {
            return;
        }
        self.entries.insert(
            (entry.epoch, entry.hash),
            Voters {
                first: entry.first,
                final_: entry.final_,
                late: entry.late,
                settled: entry.settled,
            },
        );
    }

    fn entry_of(epoch: ConsensusEpoch, hash: BlockHash, voters: &Voters) -> ManifestEntry {
        ManifestEntry {
            epoch,
            hash,
            first: voters.first,
            final_: voters.final_,
            late: voters.late,
            settled: voters.settled,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = ManifestEntry> + '_ {
        self.entries
            .iter()
            .map(|((epoch, hash), voters)| Self::entry_of(*epoch, *hash, voters))
    }

    /// The entries `from ..`, at most `max`, in canonical order
    pub fn chunk(&self, from: usize, max: usize) -> Vec<ManifestEntry> {
        self.entries().skip(from).take(max).collect()
    }

    /// `mu_e`
    pub fn digest(&self) -> BlockHash {
        let mut builder = Blake2HashBuilder::new()
            .update(b"RAI evidence manifest")
            .update((self.entries.len() as u64).to_le_bytes());
        for entry in self.entries() {
            builder = builder
                .update(entry.epoch.as_u64().to_le_bytes())
                .update(entry.hash.as_bytes())
                .update(entry.first.to_le_bytes())
                .update(entry.final_.to_le_bytes())
                .update(entry.late.to_le_bytes())
                .update(entry.settled.to_le_bytes());
        }
        builder.build()
    }

    pub fn entry(&self, epoch: ConsensusEpoch, hash: &BlockHash) -> Option<ManifestEntry> {
        self.entries
            .get(&(epoch, *hash))
            .map(|voters| Self::entry_of(epoch, *hash, voters))
    }

    /// The certificates the manifest's votes for a block form in the
    /// committee that issued the epoch's votes, counted as an election
    /// counts them
    pub fn kinds(
        &self,
        epoch: ConsensusEpoch,
        hash: &BlockHash,
        committee: &Committee,
        order: &MemberOrder,
    ) -> CertificateKinds {
        let Some(voters) = self.entries.get(&(epoch, *hash)) else {
            return CertificateKinds::default();
        };
        let thresholds = committee.thresholds();
        CertificateKinds {
            notarization: order.weight(committee, voters.first | voters.final_)
                >= thresholds.certificate,
            finalization: order.weight(committee, voters.final_) >= thresholds.certificate,
            fast: order.weight(committee, voters.settled) >= thresholds.fast,
        }
    }

    /// RAI, "exclusion witness" `XW_e(B)`: whether the manifest names `q`
    /// supporters of a block in an epoch, first votes and late
    /// notarizations in any mix (final voters first voted the block). It
    /// discharges a recovery record of that origin on a conflicting block,
    /// and is the closing-epoch evidence of the core overlap exception.
    pub fn exclusion_witness(
        &self,
        epoch: ConsensusEpoch,
        hash: &BlockHash,
        committee: &Committee,
        order: &MemberOrder,
    ) -> bool {
        self.entries.get(&(epoch, *hash)).is_some_and(|voters| {
            order.weight(committee, voters.first | voters.final_ | voters.late)
                >= committee.thresholds().certificate
        })
    }

    /// Whether the manifest names a voter's vote of a kind for a block
    pub fn names_vote(
        &self,
        epoch: ConsensusEpoch,
        voter: &PublicKey,
        hash: &BlockHash,
        kind: ResidualKind,
        order: &MemberOrder,
    ) -> bool {
        let Some(voters) = self.entries.get(&(epoch, *hash)) else {
            return false;
        };
        let Some(bit) = order.bit(voter) else {
            return false;
        };
        match kind {
            ResidualKind::First => voters.first & bit != 0,
            ResidualKind::Final => voters.final_ & bit != 0,
        }
    }

    /// The epochs the manifest names votes in
    pub fn epochs(&self) -> BTreeSet<ConsensusEpoch> {
        self.entries.keys().map(|(epoch, _)| *epoch).collect()
    }
}

/// RAI: a manifest being fetched by digest, chunk by chunk
pub struct ManifestAssembly {
    pub digest: BlockHash,
    total: u32,
    received: BTreeMap<u32, ManifestEntry>,
}

impl ManifestAssembly {
    pub fn new(digest: BlockHash) -> Self {
        Self {
            digest,
            total: 0,
            received: BTreeMap::new(),
        }
    }

    /// Takes a chunk; the next index to ask for, or the complete manifest
    /// if the digest checks out, or an error if it does not
    pub fn take(
        &mut self,
        total: u32,
        from: u32,
        entries: &[ManifestEntry],
    ) -> Result<Option<Manifest>, ()> {
        if self.received.is_empty() {
            self.total = total;
        } else if total != self.total {
            return Err(());
        }
        for (i, entry) in entries.iter().enumerate() {
            let index = from + i as u32;
            if index < self.total {
                self.received.insert(index, *entry);
            }
        }
        if self.received.len() < self.total as usize {
            return Ok(None);
        }
        let mut manifest = Manifest::new();
        for entry in self.received.values() {
            manifest.insert(*entry);
        }
        if manifest.digest() == self.digest {
            Ok(Some(manifest))
        } else {
            Err(())
        }
    }

    /// The first index not received yet
    pub fn next_from(&self) -> u32 {
        (0..self.total)
            .find(|i| !self.received.contains_key(i))
            .unwrap_or(self.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::PrivateKey;
    use rustc_hash::FxHashMap;

    /// Six equal-weight members with f = p = 1; any other size is a
    /// stake-weighted committee of unit weights
    fn committee(n: usize) -> (Committee, Vec<PublicKey>) {
        let keys: Vec<PublicKey> = (1..=n as u64)
            .map(|i| PrivateKey::from(i).public_key())
            .collect();
        let committee = if n == 6 {
            Committee::equal_weight(keys.iter().copied(), 1, 1).unwrap()
        } else {
            let weights: FxHashMap<PublicKey, Amount> =
                keys.iter().map(|k| (*k, Amount::raw(1))).collect();
            Committee::new(weights)
        };
        (committee, keys)
    }

    #[test]
    fn the_digest_is_canonical_and_entries_name_certificates() {
        let (committee, keys) = committee(6);
        let order = MemberOrder::of(&committee).unwrap();
        let epoch = ConsensusEpoch::new(2);
        let hash = BlockHash::from(9);
        let mut a = Manifest::new();
        let mut b = Manifest::new();
        let entry = ManifestEntry {
            epoch,
            hash,
            first: order.mask(keys[..4].iter().copied()),
            final_: order.mask(keys[..2].iter().copied()),
            late: 0,
            settled: order.mask(keys[..4].iter().copied()),
        };
        let other = ManifestEntry {
            epoch,
            hash: BlockHash::from(10),
            first: order.mask(keys[..5].iter().copied()),
            final_: 0,
            late: 0,
            settled: order.mask(keys[..5].iter().copied()),
        };
        a.insert(entry);
        a.insert(other);
        b.insert(other);
        b.insert(entry);
        assert_eq!(a.digest(), b.digest());
        assert_ne!(a.digest(), Manifest::new().digest());

        let kinds = a.kinds(epoch, &hash, &committee, &order);
        assert!(kinds.notarization);
        assert!(!kinds.finalization);
        assert!(!kinds.fast);
        assert!(
            a.kinds(epoch, &BlockHash::from(10), &committee, &order)
                .fast
        );
        assert!(a.names_vote(epoch, &keys[0], &hash, ResidualKind::First, &order));
        assert!(!a.names_vote(epoch, &keys[5], &hash, ResidualKind::First, &order));
        assert!(!a.names_vote(epoch, &keys[3], &hash, ResidualKind::Final, &order));
        assert_eq!(
            a.kinds(epoch, &BlockHash::from(11), &committee, &order),
            CertificateKinds::default()
        );
    }

    #[test]
    fn a_manifest_is_assembled_from_chunks_and_checked_against_its_digest() {
        let (committee, keys) = committee(6);
        let order = MemberOrder::of(&committee).unwrap();
        let mut manifest = Manifest::new();
        for i in 1..=5u64 {
            manifest.insert(ManifestEntry {
                epoch: ConsensusEpoch::new(1),
                hash: BlockHash::from(i),
                first: order.mask(keys[..4].iter().copied()),
                final_: 0,
                late: 0,
                settled: 0,
            });
        }
        let digest = manifest.digest();
        let mut assembly = ManifestAssembly::new(digest);
        let total = manifest.len() as u32;
        assert_eq!(assembly.take(total, 0, &manifest.chunk(0, 2)), Ok(None));
        assert_eq!(assembly.next_from(), 2);
        assert_eq!(assembly.take(total, 2, &manifest.chunk(2, 2)), Ok(None));
        assert_eq!(
            assembly.take(total, 4, &manifest.chunk(4, 2)),
            Ok(Some(manifest.clone()))
        );

        let mut wrong = ManifestAssembly::new(BlockHash::from(99));
        assert_eq!(wrong.take(total, 0, &manifest.chunk(0, 5)), Err(()));
    }

    /// Mixed NC: three first votes and one late notarization are an
    /// exclusion witness and no certificate; the late bit is in mu_e
    #[test]
    fn late_notarizers_count_toward_an_exclusion_witness_only() {
        let (committee, keys) = committee(6);
        let order = MemberOrder::of(&committee).unwrap();
        let epoch = ConsensusEpoch::new(1);
        let hash = BlockHash::from(9);
        let first = ManifestEntry {
            epoch,
            hash,
            first: order.mask(keys[..3].iter().copied()),
            final_: 0,
            late: 0,
            settled: 0,
        };
        let mixed = ManifestEntry {
            late: order.mask([keys[3]]),
            ..first
        };
        let mut without = Manifest::new();
        without.insert(first);
        let mut with = Manifest::new();
        with.insert(mixed);
        assert_ne!(with.digest(), without.digest());
        assert!(!without.exclusion_witness(epoch, &hash, &committee, &order));
        assert!(with.exclusion_witness(epoch, &hash, &committee, &order));
        assert_eq!(
            with.kinds(epoch, &hash, &committee, &order),
            CertificateKinds::default()
        );
        // A late-only entry is kept
        let mut late_only = Manifest::new();
        late_only.insert(ManifestEntry { first: 0, ..mixed });
        assert_eq!(late_only.len(), 1);
    }

    /// "No fast path on early votes": five first votes are a fast
    /// certificate only if five of them are settled
    #[test]
    fn a_fast_certificate_counts_settled_first_votes_only() {
        let (committee, keys) = committee(6);
        let order = MemberOrder::of(&committee).unwrap();
        let epoch = ConsensusEpoch::new(1);
        let hash = BlockHash::from(9);
        let entry = |settled: usize| ManifestEntry {
            epoch,
            hash,
            first: order.mask(keys[..5].iter().copied()),
            final_: 0,
            late: 0,
            settled: order.mask(keys[..settled].iter().copied()),
        };
        for (settled, fast) in [(0, false), (4, false), (5, true)] {
            let mut manifest = Manifest::new();
            manifest.insert(entry(settled));
            let kinds = manifest.kinds(epoch, &hash, &committee, &order);
            assert!(kinds.notarization);
            assert_eq!(kinds.fast, fast, "settled={settled}");
        }
    }

    #[test]
    fn a_committee_too_large_for_the_bit_sets_has_no_order() {
        let (committee, _) = committee(65);
        assert!(MemberOrder::of(&committee).is_none());
    }
}
