use rsnano_types::{Amount, BlockHash, DescTallyKey};
use rustc_hash::FxHashSet;
use std::collections::BTreeMap;

/// Blocks ordered by their non-final tally
#[derive(Default)]
pub(crate) struct TallyIndex {
    map: BTreeMap<DescTallyKey, FxHashSet<BlockHash>>,
}

impl TallyIndex {
    pub fn insert(&mut self, tally: Amount, hash: BlockHash) {
        self.map.entry(tally.into()).or_default().insert(hash);
    }

    pub fn update(&mut self, hash: BlockHash, old_tally: Amount, new_tally: Amount) {
        if old_tally == new_tally {
            return;
        }
        self.remove(&hash, old_tally);
        self.insert(new_tally, hash);
    }

    pub fn remove(&mut self, hash: &BlockHash, tally: Amount) {
        let key = DescTallyKey::from(tally);
        let hashes = self.map.get_mut(&key).unwrap();
        if hashes.len() == 1 {
            self.map.remove(&key);
        } else {
            hashes.remove(hash);
        }
    }

    pub fn iter_desc(&self) -> impl Iterator<Item = &BlockHash> {
        self.map.values().flat_map(|hashes| hashes.iter())
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }
}
