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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_index_has_no_entries() {
        let index = TallyIndex::default();
        assert_eq!(index.iter_desc().count(), 0);
    }

    #[test]
    fn insert_makes_hash_iterable() {
        let mut index = TallyIndex::default();
        let hash = BlockHash::from(1);

        index.insert(Amount::raw(5), hash);

        assert_eq!(index.iter_desc().collect::<Vec<_>>(), vec![&hash]);
    }

    #[test]
    fn iter_desc_orders_by_descending_tally() {
        let mut index = TallyIndex::default();
        let low = BlockHash::from(1);
        let mid = BlockHash::from(2);
        let high = BlockHash::from(3);

        index.insert(Amount::raw(5), low);
        index.insert(Amount::raw(20), high);
        index.insert(Amount::raw(10), mid);

        assert_eq!(
            index.iter_desc().collect::<Vec<_>>(),
            vec![&high, &mid, &low]
        );
    }

    #[test]
    fn multiple_hashes_with_same_tally_are_all_kept() {
        let mut index = TallyIndex::default();
        let a = BlockHash::from(1);
        let b = BlockHash::from(2);

        index.insert(Amount::raw(5), a);
        index.insert(Amount::raw(5), b);

        let hashes: FxHashSet<_> = index.iter_desc().copied().collect();
        assert_eq!(hashes, FxHashSet::from_iter([a, b]));
    }

    #[test]
    fn update_moves_hash_to_new_tally_position() {
        let mut index = TallyIndex::default();
        let moved = BlockHash::from(1);
        let other = BlockHash::from(2);
        index.insert(Amount::raw(5), moved);
        index.insert(Amount::raw(10), other);

        // Moving above `other`'s tally must change the iteration order
        index.update(moved, Amount::raw(5), Amount::raw(20));

        assert_eq!(index.iter_desc().collect::<Vec<_>>(), vec![&moved, &other]);
    }

    #[test]
    fn update_with_unchanged_tally_is_a_no_op() {
        let mut index = TallyIndex::default();
        let hash = BlockHash::from(1);
        index.insert(Amount::raw(5), hash);

        index.update(hash, Amount::raw(5), Amount::raw(5));

        assert_eq!(index.iter_desc().collect::<Vec<_>>(), vec![&hash]);
    }

    #[test]
    fn remove_last_hash_for_a_tally_drops_the_bucket() {
        let mut index = TallyIndex::default();
        let hash = BlockHash::from(1);
        index.insert(Amount::raw(5), hash);

        index.remove(&hash, Amount::raw(5));

        assert_eq!(index.iter_desc().count(), 0);
    }

    #[test]
    fn remove_one_of_several_hashes_keeps_the_others() {
        let mut index = TallyIndex::default();
        let a = BlockHash::from(1);
        let b = BlockHash::from(2);
        index.insert(Amount::raw(5), a);
        index.insert(Amount::raw(5), b);

        index.remove(&a, Amount::raw(5));

        assert_eq!(index.iter_desc().collect::<Vec<_>>(), vec![&b]);
    }

    #[test]
    fn clear_removes_all_entries() {
        let mut index = TallyIndex::default();
        index.insert(Amount::raw(5), BlockHash::from(1));
        index.insert(Amount::raw(10), BlockHash::from(2));

        index.clear();

        assert_eq!(index.iter_desc().count(), 0);
    }
}
