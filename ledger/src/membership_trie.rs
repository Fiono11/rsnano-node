use std::collections::{BTreeMap, BTreeSet, HashSet};

use rsnano_types::{Blake2HashBuilder, BlockHash};

use crate::MembershipSketch;

/// Number of level-1 buckets: members are bucketed by their first byte.
pub const LEVEL1_BUCKETS: usize = 256;
/// Bytes of a leaf digest carried in a level-2 page entry after the sub-bucket index.
const LEAF_DIGEST_BYTES: usize = 31;

/// Sorted epoch membership with a three-level radix digest tree: the root over
/// 256 level-1 digests (first byte), each over the non-empty leaves of its
/// bucket (second byte), each over the members sharing that two-byte prefix.
/// Two replicas compare roots, then the level-1 digests, then one level-2 page
/// per differing bucket, then one leaf per differing prefix, so the members
/// exchanged are proportional to the difference rather than to the membership.
/// Digests are recomputed lazily for the changed path only.
#[derive(Clone, Debug)]
pub struct MembershipTrie {
    epoch: u64,
    leaves: BTreeMap<u16, Vec<BlockHash>>,
    leaf_digests: BTreeMap<u16, BlockHash>,
    dirty_leaves: BTreeSet<u16>,
    level1: Vec<BlockHash>,
    dirty_level1: Vec<bool>,
    root: BlockHash,
    root_dirty: bool,
    len: usize,
    sketch: MembershipSketch,
}

impl MembershipTrie {
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            leaves: BTreeMap::new(),
            leaf_digests: BTreeMap::new(),
            dirty_leaves: BTreeSet::new(),
            level1: (0..LEVEL1_BUCKETS)
                .map(|bucket| level1_digest(epoch, bucket as u8, &[]))
                .collect(),
            dirty_level1: vec![false; LEVEL1_BUCKETS],
            root: BlockHash::ZERO,
            root_dirty: true,
            len: 0,
            sketch: MembershipSketch::new(),
        }
    }

    pub fn from_members(epoch: u64, members: impl IntoIterator<Item = BlockHash>) -> Self {
        let mut trie = Self::new(epoch);
        for member in members {
            trie.insert(member);
        }
        trie
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn prefix_of(hash: &BlockHash) -> u16 {
        u16::from_be_bytes([hash.as_bytes()[0], hash.as_bytes()[1]])
    }

    pub fn bucket_of(hash: &BlockHash) -> u8 {
        hash.as_bytes()[0]
    }

    /// Returns whether the member was new.
    pub fn insert(&mut self, member: BlockHash) -> bool {
        let prefix = Self::prefix_of(&member);
        let leaf = self.leaves.entry(prefix).or_default();
        let Err(position) = leaf.binary_search(&member) else {
            return false;
        };
        leaf.insert(position, member);
        self.len += 1;
        self.sketch.insert(&member);
        self.mark_dirty(prefix);
        true
    }

    /// The set sketch of the membership, maintained with every insertion.
    pub fn sketch(&self) -> &MembershipSketch {
        &self.sketch
    }

    fn mark_dirty(&mut self, prefix: u16) {
        self.dirty_leaves.insert(prefix);
        self.dirty_level1[(prefix >> 8) as usize] = true;
        self.root_dirty = true;
    }

    pub fn contains(&self, member: &BlockHash) -> bool {
        self.leaves
            .get(&Self::prefix_of(member))
            .is_some_and(|leaf| leaf.binary_search(member).is_ok())
    }

    /// Members in ascending order.
    pub fn members(&self) -> impl Iterator<Item = &BlockHash> {
        self.leaves.values().flatten()
    }

    pub fn leaf(&self, prefix: u16) -> &[BlockHash] {
        self.leaves.get(&prefix).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The membership without `excluded`, as a separate trie.
    pub fn without(&self, excluded: &HashSet<BlockHash>) -> Self {
        Self::from_members(
            self.epoch,
            self.members().filter(|m| !excluded.contains(m)).copied(),
        )
    }

    fn flush(&mut self) {
        let epoch = self.epoch;
        for prefix in std::mem::take(&mut self.dirty_leaves) {
            match self.leaves.get(&prefix).filter(|leaf| !leaf.is_empty()) {
                Some(leaf) => {
                    self.leaf_digests
                        .insert(prefix, leaf_digest(epoch, prefix, leaf));
                }
                None => {
                    self.leaf_digests.remove(&prefix);
                }
            }
        }
        for bucket in 0..LEVEL1_BUCKETS {
            if !self.dirty_level1[bucket] {
                continue;
            }
            self.dirty_level1[bucket] = false;
            let entries: Vec<_> = self
                .bucket_leaf_digests(bucket as u8)
                .map(|(prefix, digest)| (prefix as u8, *digest))
                .collect();
            self.level1[bucket] = level1_digest(epoch, bucket as u8, &entries);
        }
        if self.root_dirty {
            self.root_dirty = false;
            self.root = root_digest(epoch, &self.level1);
        }
    }

    fn bucket_leaf_digests(&self, bucket: u8) -> impl Iterator<Item = (u16, &BlockHash)> {
        let start = (bucket as u16) << 8;
        self.leaf_digests
            .range(start..=start | 0xff)
            .map(|(prefix, digest)| (*prefix, digest))
    }

    pub fn root(&mut self) -> BlockHash {
        self.flush();
        self.root
    }

    pub fn level1(&mut self) -> &[BlockHash] {
        self.flush();
        &self.level1
    }

    /// Entries of one level-1 bucket: the sub-bucket index followed by the leaf
    /// digest, one per non-empty leaf, in prefix order.
    pub fn level2(&mut self, bucket: u8) -> Vec<BlockHash> {
        self.flush();
        self.bucket_leaf_digests(bucket)
            .map(|(prefix, digest)| level2_entry(prefix as u8, digest))
            .collect()
    }

    /// Level-1 buckets whose digest differs from `theirs`.
    pub fn mismatched_buckets(&mut self, theirs: &[BlockHash]) -> Vec<u8> {
        self.flush();
        (0..LEVEL1_BUCKETS)
            .filter(|bucket| theirs.get(*bucket) != Some(&self.level1[*bucket]))
            .map(|bucket| bucket as u8)
            .collect()
    }

    /// Prefixes of `bucket` whose leaf differs from the level-2 page `theirs`:
    /// present on one side only, or present on both with different digests.
    pub fn mismatched_leaves(&mut self, bucket: u8, theirs: &[BlockHash]) -> Vec<u16> {
        self.flush();
        let ours: BTreeMap<u8, BlockHash> = self
            .bucket_leaf_digests(bucket)
            .map(|(prefix, digest)| (prefix as u8, level2_entry(prefix as u8, digest)))
            .collect();
        let theirs: BTreeMap<u8, BlockHash> = theirs
            .iter()
            .map(|entry| (entry.as_bytes()[0], *entry))
            .collect();
        ours.keys()
            .chain(theirs.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|sub| ours.get(sub) != theirs.get(sub))
            .map(|sub| (bucket as u16) << 8 | *sub as u16)
            .collect()
    }
}

/// A level-2 page entry: the sub-bucket index, then the leaf digest truncated
/// to fit, so a page stays a plain list of hashes.
pub fn level2_entry(sub_bucket: u8, leaf_digest: &BlockHash) -> BlockHash {
    let mut bytes = [0u8; 32];
    bytes[0] = sub_bucket;
    bytes[1..].copy_from_slice(&leaf_digest.as_bytes()[..LEAF_DIGEST_BYTES]);
    BlockHash::from_bytes(bytes)
}

fn leaf_digest(epoch: u64, prefix: u16, members: &[BlockHash]) -> BlockHash {
    let mut builder = Blake2HashBuilder::new()
        .update(b"RAI-CLOSE-LEAF")
        .update(epoch.to_le_bytes())
        .update(prefix.to_le_bytes());
    for member in members {
        builder = builder.update(member.as_bytes());
    }
    builder.build()
}

fn level1_digest(epoch: u64, bucket: u8, leaves: &[(u8, BlockHash)]) -> BlockHash {
    let mut builder = Blake2HashBuilder::new()
        .update(b"RAI-CLOSE-L1")
        .update(epoch.to_le_bytes())
        .update([bucket]);
    for (sub_bucket, digest) in leaves {
        builder = builder.update([*sub_bucket]).update(digest.as_bytes());
    }
    builder.build()
}

fn root_digest(epoch: u64, level1: &[BlockHash]) -> BlockHash {
    let mut builder = Blake2HashBuilder::new()
        .update(b"RAI-CLOSE-STATE")
        .update(epoch.to_le_bytes());
    for digest in level1 {
        builder = builder.update(digest.as_bytes());
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_depends_on_members_and_epoch_but_not_on_insertion_order() {
        let a = hash(0x01, 0x02, 3);
        let b = hash(0x01, 0x02, 4);
        let c = hash(0xff, 0x00, 5);
        let mut forward = MembershipTrie::from_members(7, [a, b, c]);
        let mut backward = MembershipTrie::from_members(7, [c, b, a]);
        assert_eq!(forward.root(), backward.root());
        assert_eq!(forward.len(), 3);
        assert!(!forward.insert(b), "duplicates are ignored");
        assert_eq!(forward.len(), 3);
        assert_eq!(
            forward.members().copied().collect::<Vec<_>>(),
            vec![a, b, c]
        );
        let mut other_epoch = MembershipTrie::from_members(8, [a, b, c]);
        assert_ne!(forward.root(), other_epoch.root());
        let mut empty = MembershipTrie::new(7);
        assert_ne!(empty.root(), MembershipTrie::new(8).root());
        assert_ne!(empty.root(), forward.root());
    }

    #[test]
    fn incremental_insertion_matches_a_rebuild() {
        let members: Vec<_> = (0..500u64)
            .map(|i| hash((i % 3) as u8, (i % 7) as u8, i))
            .collect();
        let mut incremental = MembershipTrie::new(1);
        for (i, member) in members.iter().enumerate() {
            incremental.insert(*member);
            if i % 50 == 0 {
                let mut rebuilt = MembershipTrie::from_members(1, members[..=i].iter().copied());
                assert_eq!(incremental.root(), rebuilt.root());
                assert_eq!(incremental.level1(), rebuilt.level1());
                assert_eq!(incremental.level2(1), rebuilt.level2(1));
            }
        }
        let mut rebuilt = MembershipTrie::from_members(1, members.iter().copied());
        assert_eq!(incremental.root(), rebuilt.root());
        assert!(incremental.contains(&members[17]));
        assert!(!incremental.contains(&hash(9, 9, 9)));
    }

    #[test]
    fn comparison_names_the_differing_buckets_leaves_and_members() {
        let shared = hash(0x10, 0x20, 1);
        let only_mine = hash(0x10, 0x21, 2);
        let only_theirs = hash(0x30, 0x40, 3);
        let mut mine = MembershipTrie::from_members(0, [shared, only_mine]);
        let mut theirs = MembershipTrie::from_members(0, [shared, only_theirs]);
        assert_ne!(mine.root(), theirs.root());
        let their_level1 = theirs.level1().to_vec();
        assert_eq!(mine.mismatched_buckets(&their_level1), vec![0x10, 0x30]);
        assert_eq!(
            mine.mismatched_leaves(0x10, &theirs.level2(0x10)),
            vec![0x1021],
            "the shared leaf matches, only my extra leaf differs"
        );
        assert_eq!(
            mine.mismatched_leaves(0x30, &theirs.level2(0x30)),
            vec![0x3040]
        );
        assert_eq!(theirs.leaf(0x3040), &[only_theirs]);
        assert!(mine.leaf(0x3040).is_empty());
        let mut reconciled = mine.without(&HashSet::from([only_mine]));
        reconciled.insert(only_theirs);
        assert_eq!(reconciled.root(), theirs.root());
        assert!(
            mine.mismatched_buckets(&[BlockHash::ZERO; 3]).len() == LEVEL1_BUCKETS,
            "a short digest list differs everywhere"
        );
    }

    #[test]
    fn level2_entries_carry_the_sub_bucket_and_truncated_digest() {
        let member = hash(0xab, 0xcd, 1);
        let mut trie = MembershipTrie::from_members(3, [member]);
        let entries = trie.level2(0xab);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].as_bytes()[0], 0xcd);
        let digest = leaf_digest(3, 0xabcd, &[member]);
        assert_eq!(&entries[0].as_bytes()[1..], &digest.as_bytes()[..31]);
        assert!(trie.level2(0xac).is_empty());
    }

    fn hash(first: u8, second: u8, tail: u64) -> BlockHash {
        let mut bytes = [0u8; 32];
        bytes[0] = first;
        bytes[1] = second;
        bytes[24..].copy_from_slice(&tail.to_be_bytes());
        BlockHash::from_bytes(bytes)
    }
}
