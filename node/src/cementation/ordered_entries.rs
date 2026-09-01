use rsnano_types::BlockHash;
use std::collections::{HashMap, VecDeque};

use super::CementingEntry;

#[derive(Default)]
pub(super) struct OrderedEntries {
    sequenced: VecDeque<BlockHash>,
    by_hash: HashMap<BlockHash, CementingEntry>,
}

impl OrderedEntries {
    pub fn push_back(&mut self, entry: CementingEntry) -> bool {
        let hash = entry.confirmation_root;
        let mut inserted = true;

        self.by_hash
            .entry(hash)
            .and_modify(|existing| {
                existing.epoch = existing.epoch.min(entry.epoch);
                inserted = false;
            })
            .or_insert(entry);

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
            self.by_hash.remove(&hash)
        } else {
            None
        }
    }

    pub(crate) fn remove(&mut self, hash: &BlockHash) -> Option<CementingEntry> {
        if let Some(entry) = self.by_hash.remove(hash) {
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
        self.by_hash.values().any(|entry| entry.epoch == epoch)
    }

    pub(crate) fn has_eligible(&self, max_epoch: u64) -> bool {
        self.by_hash
            .values()
            .any(|entry| entry.epoch == 0 || entry.epoch <= max_epoch)
    }

    pub(crate) fn pop_front_eligible(&mut self, max_epoch: u64) -> Option<CementingEntry> {
        let position = self.sequenced.iter().position(|hash| {
            self.by_hash
                .get(hash)
                .is_some_and(|entry| entry.epoch == 0 || entry.epoch <= max_epoch)
        })?;
        let hash = self.sequenced.remove(position)?;
        self.by_hash.remove(&hash)
    }
}
