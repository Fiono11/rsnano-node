use std::collections::BTreeMap;

use rustc_hash::{FxHashMap, FxHashSet};

use rsnano_types::{Amount, BlockHash, DescTallyKey};

use super::voted_block::VotedBlock;

#[derive(Default)]
pub(crate) struct VotedBlockMap {
    sequential: BTreeMap<usize, BlockHash>,
    by_hash: FxHashMap<BlockHash, VotedBlock>,
    by_tally: BTreeMap<DescTallyKey, FxHashSet<BlockHash>>,
}

impl VotedBlockMap {
    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.by_hash.contains_key(hash)
    }

    pub fn insert(&mut self, entry: VotedBlock) {
        let old = self.sequential.insert(entry.id, entry.hash);
        debug_assert!(old.is_none());

        let tally = entry.tally().into();
        self.by_tally.entry(tally).or_default().insert(entry.hash);

        let old = self.by_hash.insert(entry.hash, entry);
        debug_assert!(old.is_none());
    }

    pub fn modify_by_hash<F>(&mut self, hash: &BlockHash, f: F) -> bool
    where
        F: FnOnce(&mut VotedBlock),
    {
        if let Some(entry) = self.by_hash.get_mut(hash) {
            let old_tally = entry.tally();
            f(entry);
            let new_tally = entry.tally();
            let hash = entry.hash;
            self.update_tally(hash, old_tally, new_tally);
            true
        } else {
            false
        }
    }

    fn update_tally(&mut self, hash: BlockHash, old_tally: Amount, new_tally: Amount) {
        if old_tally == new_tally {
            return;
        }
        self.remove_by_tally(&hash, old_tally);
        self.by_tally
            .entry(new_tally.into())
            .or_default()
            .insert(hash);
    }

    fn remove_by_tally(&mut self, hash: &BlockHash, tally: Amount) {
        let key = DescTallyKey::from(tally);
        let hashes = self.by_tally.get_mut(&key).unwrap();
        if hashes.len() == 1 {
            self.by_tally.remove(&key);
        } else {
            hashes.remove(hash);
        }
    }

    pub fn pop_front(&mut self) -> Option<VotedBlock> {
        match self.sequential.pop_first() {
            Some((_, front_hash)) => {
                let entry = self.by_hash.remove(&front_hash).unwrap();
                self.remove_by_tally(&front_hash, entry.tally());
                Some(entry)
            }
            None => None,
        }
    }

    pub fn get_by_hash(&self, hash: &BlockHash) -> Option<&VotedBlock> {
        self.by_hash.get(hash)
    }

    pub fn remove_by_hash(&mut self, hash: &BlockHash) -> Option<VotedBlock> {
        match self.by_hash.remove(hash) {
            Some(entry) => {
                self.sequential.remove(&entry.id);
                self.remove_by_tally(hash, entry.tally());
                Some(entry)
            }
            None => None,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &VotedBlock> {
        self.by_hash.values()
    }

    pub fn iter_by_tally_desc(&self) -> impl Iterator<Item = &VotedBlock> {
        self.by_tally
            .values()
            .flat_map(|hashes| hashes.iter().map(|hash| self.by_hash.get(hash).unwrap()))
    }

    pub fn len(&self) -> usize {
        self.sequential.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sequential.is_empty()
    }

    pub fn clear(&mut self) {
        self.sequential.clear();
        self.by_hash.clear();
        self.by_tally.clear();
    }
}
