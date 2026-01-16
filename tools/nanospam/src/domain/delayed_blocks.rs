use std::{collections::BTreeMap, time::Duration};

use rustc_hash::FxHashMap;

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Block, BlockHash};

const DELAY_LIMIT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(crate) struct DelayedBlocks {
    /// block + publish timestamp
    blocks: FxHashMap<BlockHash, PublishInfo>,
    by_time: BTreeMap<Timestamp, Vec<BlockHash>>,
}

impl DelayedBlocks {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Default::default()
    }

    pub fn next(&mut self, now: Timestamp) -> Option<Block> {
        let mut entry = self.by_time.first_entry()?;
        let sent = *entry.key();
        let block_hashes = entry.get_mut();
        if now < sent + DELAY_LIMIT {
            return None;
        }

        let hash = block_hashes.pop().unwrap();
        if block_hashes.is_empty() {
            entry.remove();
        }

        let info = self.blocks.get_mut(&hash).unwrap();
        info.last_publish = None;
        Some(info.block.clone())
    }

    pub fn insert(&mut self, block: Block) {
        let hash = block.hash();
        tracing::debug!("DelayedBlocks::insert - adding block {hash} to tracker");
        if let Some(info) = self.blocks.insert(hash, PublishInfo::new(block))
            && let Some(old_sent) = info.last_publish
        {
            self.remove_from_time_index(&hash, old_sent);
        }
    }

    pub fn published(&mut self, hash: &BlockHash, timestamp: Timestamp) {
        if let Some(info) = self.blocks.get_mut(hash) {
            if info.first_publish.is_none() {
                info.first_publish = Some(timestamp);
                tracing::debug!(
                    "DelayedBlocks::published - block {hash} first published at {:?}",
                    timestamp
                );
            }
            let old_sent = info.last_publish;
            info.last_publish = Some(timestamp);

            if let Some(old_sent) = old_sent {
                self.remove_from_time_index(hash, old_sent);
            }
            self.by_time.entry(timestamp).or_default().push(*hash);
        } else {
            tracing::warn!(
                "DelayedBlocks::published - block {hash} not found in tracker when marking as published"
            );
        }
    }

    pub fn confirmed(&mut self, hash: &BlockHash, timestamp: Timestamp) -> Option<Duration> {
        if let Some(info) = self.blocks.remove(hash) {
            tracing::debug!("DelayedBlocks::confirmed - found block {hash} in tracker");
            if let Some(sent) = info.last_publish {
                self.remove_from_time_index(hash, sent);
            }
            // Use first_publish if available, otherwise fall back to last_publish
            let publish_time = info.first_publish.or(info.last_publish);
            let conf_time = publish_time.map(|i| i.elapsed(timestamp));
            tracing::info!(
                "DelayedBlocks::confirmed - block {hash} had first_publish: {:?}, last_publish: {:?}, conf_time: {:?} ({} ms)",
                info.first_publish,
                info.last_publish,
                conf_time,
                conf_time.map(|d| d.as_millis()).unwrap_or(0)
            );
            conf_time
        } else {
            let tracked_hashes: Vec<String> = self.blocks.keys().map(|h| h.to_string()).collect();
            tracing::warn!(
                "DelayedBlocks::confirmed - block {hash} not found in delayed blocks (tracking {} blocks: {:?})",
                self.blocks.len(),
                tracked_hashes
            );
            None
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    fn remove_from_time_index(&mut self, hash: &BlockHash, sent: Timestamp) {
        let mut hashes = self.by_time.remove(&sent).unwrap();
        hashes.retain(|h| h != hash);
        if !hashes.is_empty() {
            self.by_time.insert(sent, hashes);
        }
    }
}

struct PublishInfo {
    block: Block,
    first_publish: Option<Timestamp>,
    last_publish: Option<Timestamp>,
}

impl PublishInfo {
    fn new(block: Block) -> Self {
        Self {
            block,
            first_publish: None,
            last_publish: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty() {
        let mut delayed = DelayedBlocks::new();
        let now = Timestamp::new_test_instance();
        assert!(delayed.next(now).is_none());
        assert_eq!(delayed.len(), 0);
    }

    #[test]
    fn add_block() {
        let mut delayed = DelayedBlocks::new();
        let now = Timestamp::new_test_instance();
        let block = Block::new_test_instance();
        let hash = block.hash();
        delayed.insert(block);
        delayed.published(&hash, now);
        assert!(delayed.next(now + Duration::from_millis(500)).is_none());
        assert_eq!(delayed.len(), 1);
    }

    #[test]
    fn get_delayed_block() {
        let mut delayed = DelayedBlocks::new();
        let now = Timestamp::new_test_instance();
        let block = Block::new_test_instance();
        let hash = block.hash();
        delayed.insert(block);
        delayed.published(&hash, now);

        let delayed_block = delayed.next(now + DELAY_LIMIT).unwrap();

        assert_eq!(delayed_block.hash(), hash);
        assert_eq!(delayed.len(), 1);
        assert!(delayed.next(now + DELAY_LIMIT).is_none());
    }

    #[test]
    fn remove_confirmed_block() {
        let mut delayed = DelayedBlocks::new();
        let now = Timestamp::new_test_instance();
        let block = Block::new_test_instance();
        let hash = block.hash();
        delayed.insert(block);
        delayed.published(&hash, now);

        delayed.confirmed(&hash, now);

        assert_eq!(delayed.len(), 0);
    }

    #[test]
    fn update_block_time_when_inserted_twice() {
        let mut delayed = DelayedBlocks::new();
        let time_a = Timestamp::new_test_instance();
        let time_b = time_a + Duration::from_secs(1);
        let block = Block::new_test_instance();

        delayed.insert(block.clone());
        delayed.insert(block.clone());
        delayed.published(&block.hash(), time_a);
        delayed.published(&block.hash(), time_b);

        assert_eq!(delayed.len(), 1);
        assert_eq!(delayed.by_time.len(), 1);
        assert_eq!(*delayed.by_time.first_key_value().unwrap().0, time_b);
    }

    #[test]
    fn allow_multiple_blocks_with_same_sent_timestamp() {
        let mut delayed = DelayedBlocks::new();
        let now = Timestamp::new_test_instance();
        let block_a = Block::new_test_instance_with_key(1);
        let block_b = Block::new_test_instance_with_key(2);
        delayed.insert(block_a.clone());
        delayed.insert(block_b.clone());
        delayed.published(&block_a.hash(), now);
        delayed.published(&block_b.hash(), now);
        assert_eq!(delayed.len(), 2);
        assert_eq!(delayed.by_time.len(), 1);

        let popped_a = delayed.next(now + DELAY_LIMIT).unwrap();
        let popped_b = delayed.next(now + DELAY_LIMIT).unwrap();
        assert!(delayed.next(now + DELAY_LIMIT).is_none());

        assert_eq!(popped_a.hash(), block_b.hash());
        assert_eq!(popped_b.hash(), block_a.hash());
    }

    #[test]
    fn confirm_blocks_with_same_sent_timestamp() {
        let mut delayed = DelayedBlocks::new();
        let now = Timestamp::new_test_instance();
        let block_a = Block::new_test_instance_with_key(1);
        let block_b = Block::new_test_instance_with_key(2);
        delayed.insert(block_a.clone());
        delayed.insert(block_b.clone());
        delayed.published(&block_a.hash(), now);
        delayed.published(&block_b.hash(), now);

        delayed.confirmed(&block_a.hash(), now);

        assert_eq!(
            delayed.next(now + DELAY_LIMIT).unwrap().hash(),
            block_b.hash()
        );
    }
}
