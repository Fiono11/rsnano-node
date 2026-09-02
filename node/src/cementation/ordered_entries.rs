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
