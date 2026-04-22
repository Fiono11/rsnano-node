mod reenqueuer;

pub use reenqueuer::UncheckedBlockReenqueuer;

use std::{cmp::Ordering, collections::BTreeMap};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Block, BlockHash};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

/// A map of unchecked blocks and the hash of their missing dependency block
#[derive(Clone)]
pub struct UncheckedMap {
    next_id: usize,
    by_key: BTreeMap<UncheckedKey, usize>,
    by_id: BTreeMap<usize, Entry>,
    max_blocks: usize,
    inserted: u64,
    duplicate: u64,
}

impl UncheckedMap {
    pub fn new(max_blocks: usize) -> Self {
        Self {
            by_id: BTreeMap::new(),
            by_key: BTreeMap::new(),
            next_id: 0,
            max_blocks,
            inserted: 0,
            duplicate: 0,
        }
    }

    pub fn put(&mut self, dependency: BlockHash, block: Block, now: Timestamp) {
        let key = UncheckedKey::new(dependency, block.hash());
        let inserted = self.insert(Entry::new(key, block, now));
        if self.len() > self.max_blocks {
            self.pop_front();
        }
        if inserted {
            self.inserted += 1;
        } else {
            self.duplicate += 1;
        }
    }

    fn insert(&mut self, entry: Entry) -> bool {
        match self.by_key.get(&entry.key) {
            Some(_key) => false,
            None => {
                self.by_key.insert(entry.key.clone(), self.next_id);
                self.by_id.insert(self.next_id, entry);

                self.next_id = self.next_id.wrapping_add(1);

                true
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn remove(&mut self, dependency_hash: BlockHash, unchecked_hash: BlockHash) {
        let key = UncheckedKey::new(dependency_hash, unchecked_hash);
        if let Some(id) = self.by_key.remove(&key) {
            self.by_id.remove(&id);
        }
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    fn pop_front(&mut self) -> Option<Entry> {
        if let Some((_id, entry)) = self.by_id.pop_first() {
            self.by_key.remove(&entry.key);
            Some(entry)
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.by_id.clear();
        self.by_key.clear();
        self.next_id = 0;
    }

    pub fn contains_dependency(&self, dependency_hash: BlockHash) -> bool {
        self.blocks_dependend_on(dependency_hash).next().is_some()
    }

    pub fn pop_dependend_blocks(&mut self, dependency_hash: BlockHash, result: &mut Vec<Block>) {
        let start = result.len();
        result.extend(self.blocks_dependend_on(dependency_hash).cloned());
        for block in &result[start..] {
            self.remove(dependency_hash, block.hash());
        }
    }

    pub fn blocks_dependend_on(&self, dependency_hash: BlockHash) -> impl Iterator<Item = &Block> {
        self.by_key
            .range(UncheckedKey::new(dependency_hash, BlockHash::ZERO)..)
            .take_while(move |(k, _)| k.dependency_hash == dependency_hash)
            .map(|(_, id)| &self.by_id.get(id).unwrap().block)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&BlockHash, &Block)> {
        self.by_id
            .values()
            .map(|i| (&i.key.dependency_hash, &i.block))
    }

    pub fn iter_start(
        &self,
        start_dependency: BlockHash,
    ) -> impl Iterator<Item = (&BlockHash, &Block)> {
        self.by_key
            .range(UncheckedKey::new(start_dependency, BlockHash::ZERO)..)
            .map(|(key, id)| (&key.dependency_hash, &self.by_id.get(id).unwrap().block))
    }

    pub fn discard_old_entries(&mut self, cutoff: Timestamp) {
        while let Some(entry) = self.by_id.first_entry() {
            if entry.get().added <= cutoff {
                let e = entry.remove();
                self.by_key.remove(&e.key);
            } else {
                break;
            }
        }
    }
}

impl Default for UncheckedMap {
    fn default() -> Self {
        Self::new(1024 * 64)
    }
}

impl ContainerInfoProvider for UncheckedMap {
    fn container_info(&self) -> ContainerInfo {
        [("entries", self.len(), 0)].into()
    }
}

impl StatsSource for UncheckedMap {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert("unchecked", "put", self.inserted);
        result.insert("unchecked", "duplicate", self.duplicate);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct UncheckedKey {
    /// Hash of the unfulfilled dependency (corresponding send block or previous block)
    pub dependency_hash: BlockHash,
    /// Hash of the unchceked block
    pub unchecked_hash: BlockHash,
}

impl UncheckedKey {
    pub fn new(dependency_hash: BlockHash, unchecked_hash: BlockHash) -> Self {
        Self {
            dependency_hash,
            unchecked_hash,
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    key: UncheckedKey,
    block: Block,
    added: Timestamp,
}

impl Entry {
    pub fn new(key: UncheckedKey, block: Block, added: Timestamp) -> Self {
        Self { key, block, added }
    }
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.key.eq(&other.key)
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntest::assert_false;
    use rsnano_types::Block;

    #[test]
    fn empty() {
        let unchecked = UncheckedMap::default();
        assert_eq!(unchecked.next_id, 0);
        assert_eq!(unchecked.by_id.len(), 0);
        assert_eq!(unchecked.by_key.len(), 0);
        assert_eq!(unchecked.len(), 0);
        assert_false!(unchecked.contains_dependency(1.into()));
        assert!(unchecked.is_empty());
        assert_eq!(unchecked.blocks_dependend_on(1.into()).count(), 0);
        assert!(unchecked.iter().next().is_none());
    }

    #[test]
    fn insert_one_entry() {
        let mut unchecked = UncheckedMap::default();

        let entry = test_entry(1);
        let new_insert = unchecked.insert(entry.clone());

        assert_eq!(unchecked.next_id, 1);
        assert_eq!(unchecked.by_id.len(), 1);
        assert_eq!(unchecked.by_id.get(&0).unwrap(), &entry);
        assert_eq!(unchecked.by_key.len(), 1);
        assert_eq!(unchecked.by_key.get(&entry.key).unwrap(), &0);
        assert_eq!(new_insert, true);
        assert!(unchecked.contains_dependency(entry.key.dependency_hash));
        assert_eq!(
            unchecked
                .blocks_dependend_on(entry.key.dependency_hash)
                .count(),
            1
        );
        assert_eq!(unchecked.iter().count(), 1);
    }

    #[test]
    fn insert_two_entries_with_same_key() {
        let mut unchecked = UncheckedMap::default();

        let entry = test_entry(1);
        let new_insert1 = unchecked.insert(entry.clone());
        let new_insert2 = unchecked.insert(entry);

        assert_eq!(unchecked.next_id, 1);
        assert_eq!(unchecked.by_id.len(), 1);
        assert_eq!(unchecked.by_key.len(), 1);
        assert_eq!(new_insert1, true);
        assert_eq!(new_insert2, false);
        assert_eq!(unchecked.iter().count(), 1);
    }

    #[test]
    fn insert_two_entries_with_different_key() {
        let mut unchecked = UncheckedMap::default();

        let new_insert1 = unchecked.insert(test_entry(1));
        let new_insert2 = unchecked.insert(test_entry(2));

        assert_eq!(unchecked.next_id, 2);
        assert_eq!(unchecked.by_id.len(), 2);
        assert_eq!(unchecked.by_key.len(), 2);
        assert_eq!(new_insert1, true);
        assert_eq!(new_insert2, true);
        assert!(unchecked.contains_dependency(1.into()));
        assert!(unchecked.contains_dependency(2.into()));
        assert_eq!(unchecked.iter().count(), 2);
    }

    #[test]
    fn pop_front() {
        let mut unchecked = UncheckedMap::default();

        unchecked.insert(test_entry(1));
        let entry = test_entry(2);
        unchecked.insert(entry.clone());

        unchecked.pop_front();

        assert_eq!(unchecked.next_id, 2);
        assert_eq!(unchecked.by_id.len(), 1);
        assert_eq!(unchecked.by_id.get(&1).is_some(), true);
        assert_eq!(unchecked.by_key.len(), 1);
        assert_eq!(unchecked.by_key.get(&entry.key).unwrap(), &1);
        assert_eq!(unchecked.len(), 1);
    }

    #[test]
    fn pop_front_twice() {
        let mut unchecked = UncheckedMap::default();

        unchecked.insert(test_entry(1));
        unchecked.insert(test_entry(2));

        unchecked.pop_front();
        unchecked.pop_front();

        assert_eq!(unchecked.len(), 0);
    }

    #[test]
    fn remove_by_key() {
        let mut unchecked = UncheckedMap::default();
        unchecked.insert(test_entry(1));
        let entry = test_entry(2);
        unchecked.insert(entry.clone());

        unchecked.remove(entry.key.dependency_hash, entry.key.unchecked_hash);

        assert_eq!(unchecked.len(), 1);
        assert_eq!(unchecked.by_id.len(), 1);
        assert_eq!(unchecked.by_key.len(), 1);
    }

    fn test_entry<T: Into<BlockHash>>(hash: T) -> Entry {
        Entry::new(
            UncheckedKey::new(hash.into(), BlockHash::default()),
            Block::new_test_instance(),
            Timestamp::new_test_instance(),
        )
    }
}
