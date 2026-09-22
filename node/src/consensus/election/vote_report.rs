use std::collections::BTreeMap;

use rsnano_types::{Account, Blake2HashBuilder, BlockHash, ConsensusEpoch, PublicKey};

/// RAI, Section 6.1: which of a replica's two one-shot votes in a slot an
/// entry of its report stands for
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReportKind {
    First,
    Final,
}

impl ReportKind {
    fn as_byte(self) -> u8 {
        match self {
            ReportKind::First => 0,
            ReportKind::Final => 1,
        }
    }
}

/// RAI, Section 6.1: the key of a report entry. The paper's (j, v, t) - tree,
/// slot, vote type - is (account, height, kind) here: an account's chain is a
/// tree and its height is the slot within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReportKey {
    pub account: Account,
    pub height: u64,
    pub kind: ReportKind,
}

impl ReportKey {
    /// The bounds of a bucket's range in the entry map
    pub(crate) const MIN: ReportKey = ReportKey {
        account: Account::ZERO,
        height: 0,
        kind: ReportKind::First,
    };
    pub(crate) const MAX: ReportKey = ReportKey {
        account: Account::MAX,
        height: u64::MAX,
        kind: ReportKind::Final,
    };

    pub fn new(account: Account, height: u64, kind: ReportKind) -> Self {
        Self {
            account,
            height,
            kind,
        }
    }

    /// The bucket the entry falls in, as the first byte of the key's hash:
    /// the partition is by the hash, so the entries spread evenly over the
    /// buckets whatever the accounts look like
    fn bucket(&self) -> u8 {
        self.digest_of(&BlockHash::ZERO, b"RAI report key")[0]
    }

    /// The entry's digest: the key and the value together, so a value
    /// changing changes its bucket's digest
    fn entry_digest(&self, value: &BlockHash) -> [u8; 32] {
        self.digest_of(value, b"RAI report entry")
    }

    fn digest_of(&self, value: &BlockHash, domain: &[u8]) -> [u8; 32] {
        *Blake2HashBuilder::new()
            .update(domain)
            .update(self.account.as_bytes())
            .update(self.height.to_le_bytes())
            .update([self.kind.as_byte()])
            .update(value.as_bytes())
            .build()
            .as_bytes()
    }
}

/// The buckets the key space is partitioned into
pub const BUCKETS: usize = 256;

/// RAI, Section 6: the authenticated map of the first and final votes one
/// replica issued during one epoch, keyed by (account, height, kind). The map
/// may hold tens of thousands of entries while the signed report stays
/// constant size: what is signed is its root.
///
/// The authenticated dictionary is one level of 256 buckets over the hash of
/// the key. A bucket's digest is the XOR of its entries' digests, so an entry
/// is added in constant time and the digest does not depend on the order the
/// entries arrived in; the root hashes the 256 bucket digests. Two replicas
/// holding the same entries hold the same root, whatever order they collected
/// them in.
///
/// Reconciliation (Section 6.2) compares the roots, then the bucket digests,
/// and fetches the entries of the buckets that differ - a whole bucket at a
/// time, not entry by entry. A measured epoch of the benchmark holds ~31000
/// entries and two replicas differ in ~1000-2700 of them, spread over the
/// whole key space, so nearly every bucket differs and a reconciliation
/// transfers most of the map. The paper's Merkle search tree behaves the same
/// way for a difference that is spread out; what bounds the transfer by the
/// size of the difference is set reconciliation (an IBLT), which is where
/// this goes when the close starts to depend on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteReport {
    epoch: ConsensusEpoch,
    /// The entries, keyed by their bucket first: one bucket is a range, and
    /// that range is the unit a reconciliation fetches
    entries: BTreeMap<(u8, ReportKey), BlockHash>,
    /// The XOR digest of each bucket. Only the buckets with an entry are
    /// held: an empty bucket's digest is zero.
    buckets: BTreeMap<u8, [u8; 32]>,
}

impl VoteReport {
    pub fn new(epoch: ConsensusEpoch) -> Self {
        Self {
            epoch,
            entries: BTreeMap::new(),
            buckets: BTreeMap::new(),
        }
    }

    pub fn epoch(&self) -> ConsensusEpoch {
        self.epoch
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &ReportKey) -> Option<BlockHash> {
        self.entries.get(&(key.bucket(), *key)).copied()
    }

    /// Every entry of the map, in bucket order
    pub fn entries(&self) -> impl Iterator<Item = (&ReportKey, &BlockHash)> {
        self.entries.iter().map(|((_, key), value)| (key, value))
    }

    /// Records the vote of a slot. A correct replica issues at most one first
    /// vote and at most one final vote per slot, so an entry is written once;
    /// a second write for the same key is ignored, which keeps the map the
    /// record of what was actually issued.
    pub fn add(&mut self, key: ReportKey, value: BlockHash) -> bool {
        let bucket = key.bucket();
        if self.entries.contains_key(&(bucket, key)) {
            return false;
        }
        let digest = self.buckets.entry(bucket).or_insert([0; 32]);
        for (d, e) in digest.iter_mut().zip(key.entry_digest(&value)) {
            *d ^= e;
        }
        self.entries.insert((bucket, key), value);
        true
    }

    /// The root of the map: what the report signs
    pub fn root(&self) -> BlockHash {
        let mut builder = Blake2HashBuilder::new().update(b"RAI report root");
        for digest in self.bucket_digests() {
            builder = builder.update(digest);
        }
        builder.build()
    }

    /// The digest of every bucket, the first step of a reconciliation
    pub fn bucket_digests(&self) -> Vec<[u8; 32]> {
        let mut result = vec![[0u8; 32]; BUCKETS];
        for (bucket, digest) in &self.buckets {
            result[*bucket as usize] = *digest;
        }
        result
    }

    /// The entries of one bucket, what a reconciliation fetches
    pub fn bucket_entries(&self, bucket: u8) -> Vec<(ReportKey, BlockHash)> {
        self.entries
            .range((bucket, ReportKey::MIN)..=(bucket, ReportKey::MAX))
            .map(|((_, key), value)| (*key, *value))
            .collect()
    }

    /// The buckets whose digests differ from the ones given: what a
    /// reconciliation against that map has to fetch
    pub fn differing_buckets(&self, theirs: &[[u8; 32]]) -> Vec<u8> {
        let ours = self.bucket_digests();
        (0..BUCKETS)
            .filter(|i| theirs.get(*i).copied().unwrap_or([0; 32]) != ours[*i])
            .map(|i| i as u8)
            .collect()
    }

    /// Takes the entries of one bucket of another replica's map into this
    /// one, replacing what this map held for that bucket. The caller checks
    /// the reconstructed root against the signed one afterwards; an entry
    /// that does not belong to the bucket is refused here already.
    pub fn absorb_bucket(
        &mut self,
        bucket: u8,
        entries: impl IntoIterator<Item = (ReportKey, BlockHash)>,
    ) -> bool {
        for (key, _) in self.bucket_entries(bucket) {
            self.remove(&key);
        }
        for (key, value) in entries {
            if key.bucket() != bucket {
                return false;
            }
            self.add(key, value);
        }
        true
    }

    fn remove(&mut self, key: &ReportKey) {
        let bucket = key.bucket();
        let Some(value) = self.entries.remove(&(bucket, *key)) else {
            return;
        };
        if let Some(digest) = self.buckets.get_mut(&bucket) {
            for (d, e) in digest.iter_mut().zip(key.entry_digest(&value)) {
                *d ^= e;
            }
        }
    }
}

/// RAI, Section 6.1: the report message itself, `Sign_i(REPORT, e, H(O_e), r)`.
/// It carries no block, no certificate and no vote signature: only the root of
/// the reporter's map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedReport {
    pub epoch: ConsensusEpoch,
    /// H(O_e): the digest of the old committee, the one that issued the
    /// epoch's ordinary votes
    pub committee: BlockHash,
    pub root: BlockHash,
    pub reporter: PublicKey,
}

impl SignedReport {
    /// What the reporter signs. The reporter is part of it, so a signature
    /// can not be lifted from one replica's report onto another's.
    pub fn payload(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI report")
            .update(self.epoch.as_u64().to_le_bytes())
            .update(self.committee.as_bytes())
            .update(self.root.as_bytes())
            .update(self.reporter.as_bytes())
            .build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_report_has_the_empty_root() {
        let report = VoteReport::new(ConsensusEpoch::ZERO);
        assert!(report.is_empty());
        assert_eq!(
            report.root(),
            VoteReport::new(ConsensusEpoch::new(7)).root()
        );
        assert!(report.bucket_digests().iter().all(|d| *d == [0; 32]));
    }

    /// The root commits to the entries and to nothing else: two replicas
    /// which issued the same votes hold the same root, whatever order the
    /// votes were cast in
    #[test]
    fn the_root_is_the_entries_and_not_their_order() {
        let mut one = VoteReport::new(ConsensusEpoch::ZERO);
        let mut other = VoteReport::new(ConsensusEpoch::ZERO);
        for i in 0..50 {
            one.add(first(i), BlockHash::from(i));
            one.add(final_(i), BlockHash::from(i));
        }
        for i in (0..50).rev() {
            other.add(final_(i), BlockHash::from(i));
            other.add(first(i), BlockHash::from(i));
        }
        assert_eq!(one.len(), 100);
        assert_eq!(one.root(), other.root());

        // A different value at one key, a different root
        let mut third = other.clone();
        third.remove(&first(7));
        third.add(first(7), BlockHash::from(999));
        assert_ne!(third.root(), one.root());

        // A missing entry, a different root
        let mut fourth = other.clone();
        fourth.remove(&final_(9));
        assert_ne!(fourth.root(), one.root());
    }

    /// A correct replica issues one first vote and one final vote per slot:
    /// the entry is written once and stays what was issued
    #[test]
    fn an_entry_is_written_once() {
        let mut report = VoteReport::new(ConsensusEpoch::ZERO);
        assert!(report.add(first(1), BlockHash::from(1)));
        assert!(!report.add(first(1), BlockHash::from(2)));
        assert_eq!(report.get(&first(1)), Some(BlockHash::from(1)));
        assert!(report.add(final_(1), BlockHash::from(1)));
        assert_eq!(report.len(), 2);
    }

    /// Section 6.2: the reconciliation fetches the buckets that differ, whole,
    /// and ends with the same map and the same root
    #[test]
    fn reconciliation_recovers_the_missing_entries() {
        let mut theirs = VoteReport::new(ConsensusEpoch::ZERO);
        for i in 0..200 {
            theirs.add(first(i), BlockHash::from(i));
        }
        // This replica knows most of the votes already, misses three and
        // holds one the reporter does not
        let mut ours = VoteReport::new(ConsensusEpoch::ZERO);
        for i in 0..200 {
            if i != 5 && i != 77 && i != 150 {
                ours.add(first(i), BlockHash::from(i));
            }
        }
        ours.add(first(500), BlockHash::from(500));

        let differing = ours.differing_buckets(&theirs.bucket_digests());
        assert!(!differing.is_empty());
        assert!(differing.len() <= 4, "four entries differ: {differing:?}");
        for bucket in differing {
            assert!(ours.absorb_bucket(bucket, theirs.bucket_entries(bucket)));
        }
        assert_eq!(ours.root(), theirs.root());
        assert_eq!(ours.len(), theirs.len());
        assert_eq!(ours.get(&first(77)), Some(BlockHash::from(77)));
        assert_eq!(ours.get(&first(500)), None);
    }

    /// The full-report fallback: reconciling from an empty map recovers
    /// everything and gives the signed root
    #[test]
    fn reconciliation_from_an_empty_map_recovers_everything() {
        let mut theirs = VoteReport::new(ConsensusEpoch::ZERO);
        for i in 0..100 {
            theirs.add(first(i), BlockHash::from(i));
        }
        let mut ours = VoteReport::new(ConsensusEpoch::ZERO);
        for bucket in ours.differing_buckets(&theirs.bucket_digests()) {
            assert!(ours.absorb_bucket(bucket, theirs.bucket_entries(bucket)));
        }
        assert_eq!(ours.root(), theirs.root());
        assert_eq!(ours.len(), 100);
    }

    /// Every entry is checked against its key (Lemma 6.1): an entry offered
    /// for a bucket it does not belong to is refused
    #[test]
    fn an_entry_of_another_bucket_is_refused() {
        let mut report = VoteReport::new(ConsensusEpoch::ZERO);
        let key = first(1);
        let other = key.bucket().wrapping_add(1);
        assert!(!report.absorb_bucket(other, vec![(key, BlockHash::from(1))]));
    }

    #[test]
    fn the_signed_payload_covers_epoch_committee_root_and_reporter() {
        let report = SignedReport {
            epoch: ConsensusEpoch::new(3),
            committee: BlockHash::from(7),
            root: BlockHash::from(9),
            reporter: PublicKey::from(1),
        };
        let mut other_epoch = report.clone();
        other_epoch.epoch = ConsensusEpoch::new(4);
        let mut other_committee = report.clone();
        other_committee.committee = BlockHash::from(8);
        let mut other_root = report.clone();
        other_root.root = BlockHash::from(10);
        let mut other_reporter = report.clone();
        other_reporter.reporter = PublicKey::from(2);
        assert_ne!(report.payload(), other_epoch.payload());
        assert_ne!(report.payload(), other_committee.payload());
        assert_ne!(report.payload(), other_root.payload());
        assert_ne!(report.payload(), other_reporter.payload());
    }

    /*
     * Test helpers
     */

    fn first(i: u64) -> ReportKey {
        ReportKey::new(Account::from(i), 1 + i % 3, ReportKind::First)
    }

    fn final_(i: u64) -> ReportKey {
        ReportKey::new(Account::from(i), 1 + i % 3, ReportKind::Final)
    }
}
