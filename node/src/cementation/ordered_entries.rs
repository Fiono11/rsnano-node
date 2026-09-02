use rsnano_types::BlockHash;
use std::collections::{BTreeMap, HashMap, VecDeque};

use super::CementingEntry;

#[derive(Default)]
pub(super) struct OrderedEntries {
    sequenced: VecDeque<BlockHash>,
    by_hash: HashMap<BlockHash, CementingEntry>,
    epoch_counts: BTreeMap<u64, usize>,
}

impl OrderedEntries {
    pub fn push_back(&mut self, entry: CementingEntry) -> bool {
        let hash = entry.confirmation_root;
        let mut inserted = true;

        if let Some(existing) = self.by_hash.get_mut(&hash) {
            let previous_epoch = existing.epoch;
            existing.epoch = existing.epoch.min(entry.epoch);
            inserted = false;
            if existing.epoch != previous_epoch {
                let new_epoch = existing.epoch;
                self.decrement_epoch(previous_epoch);
                *self.epoch_counts.entry(new_epoch).or_default() += 1;
            }
        } else {
            *self.epoch_counts.entry(entry.epoch).or_default() += 1;
            self.by_hash.insert(hash, entry);
        }

        if inserted {
            self.sequenced.push_back(hash);
        }

        inserted
    }

    pub(crate) fn contains(&self, hash: &BlockHash) -> bool {
        self.by_hash.contains_key(hash)
    }

    pub(crate) fn len(&self) -> usize {
        self.sequenced.len()
    }

    pub(crate) fn front(&mut self) -> Option<&CementingEntry> {
        if let Some(hash) = self.sequenced.front() {
            self.by_hash.get(hash)
        } else {
            None
        }
    }

    pub(crate) fn pop_front(&mut self) -> Option<CementingEntry> {
        if let Some(hash) = self.sequenced.pop_front() {
            let entry = self.by_hash.remove(&hash)?;
            self.decrement_epoch(entry.epoch);
            Some(entry)
        } else {
            None
        }
    }

    pub(crate) fn remove(&mut self, hash: &BlockHash) -> Option<CementingEntry> {
        if let Some(entry) = self.by_hash.remove(hash) {
            self.decrement_epoch(entry.epoch);
            self.sequenced.retain(|h| *h != entry.confirmation_root);
            Some(entry)
        } else {
            None
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sequenced.is_empty()
    }

    pub(crate) fn contains_epoch(&self, epoch: u64) -> bool {
        self.epoch_counts.contains_key(&epoch)
    }

    pub(crate) fn has_eligible(&self, max_epoch: u64) -> bool {
        self.epoch_counts
            .first_key_value()
            .is_some_and(|(epoch, _)| *epoch <= max_epoch)
    }

    pub(crate) fn pop_front_eligible(&mut self, max_epoch: u64) -> Option<CementingEntry> {
        let position = self.sequenced.iter().position(|hash| {
            self.by_hash
                .get(hash)
                .is_some_and(|entry| entry.epoch == 0 || entry.epoch <= max_epoch)
        })?;
        let hash = self.sequenced.remove(position)?;
        let entry = self.by_hash.remove(&hash)?;
        self.decrement_epoch(entry.epoch);
        Some(entry)
    }

    fn decrement_epoch(&mut self, epoch: u64) {
        let count = self
            .epoch_counts
            .get_mut(&epoch)
            .expect("cementation epoch count must match entries");
        *count -= 1;
        if *count == 0 {
            self.epoch_counts.remove(&epoch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn entry(hash: u64, epoch: u64) -> CementingEntry {
        CementingEntry {
            confirmation_root: BlockHash::from(hash),
            epoch,
            timestamp: Instant::now(),
        }
    }

    #[test]
    fn tracks_eligible_epochs_across_mutations() {
        let mut entries = OrderedEntries::default();
        entries.push_back(entry(1, 2));
        entries.push_back(entry(2, 1));
        assert!(!entries.has_eligible(0));
        assert!(entries.has_eligible(1));
        assert!(entries.contains_epoch(2));

        assert_eq!(
            entries.pop_front_eligible(1).unwrap().confirmation_root,
            BlockHash::from(2)
        );
        assert!(!entries.contains_epoch(1));
        assert!(!entries.has_eligible(1));
        entries.remove(&BlockHash::from(1));
        assert!(!entries.has_eligible(u64::MAX));
    }

    #[test]
    fn lowering_duplicate_epoch_updates_index() {
        let mut entries = OrderedEntries::default();
        entries.push_back(entry(1, 3));
        entries.push_back(entry(1, 1));
        assert!(!entries.contains_epoch(3));
        assert!(entries.contains_epoch(1));
        assert_eq!(entries.len(), 1);
    }
}
