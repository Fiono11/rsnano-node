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
                inserted = false;
                #[cfg(feature = "rai_protocol")]
                if let Some(epoch) = entry.finalization_epoch {
                    // An unchanged account frontier can occur in several
                    // consecutive close records. Attribute the block to the
                    // first epoch which certified it while still upgrading an
                    // ordinary pending confirmation with certificate data.
                    existing.finalization_epoch = Some(
                        existing
                            .finalization_epoch
                            .map_or(epoch, |existing_epoch| existing_epoch.min(epoch)),
                    );
                }
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
}
