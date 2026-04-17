use std::collections::VecDeque;

use rsnano_types::{Account, Block, BlockHash};
use rustc_hash::FxHashMap;

use super::single_block_account_set::SingleBlockAccountSet;

pub(super) struct ProcessingFinished {
    pub account: Account,
    /// `Some(hash)` means more blocks remain (now queued as ready to process).
    /// `None` means all blocks were processed; the account should be re-enqueued for download.
    pub next_block_hash: Option<BlockHash>,
}

/// Manages downloaded blocks from receipt through block-processor hand-off.
///
/// Blocks survive a block/unblock cycle: when an account is blocked its blocks
/// stay in the cache and are processed immediately once the account is unblocked,
/// without re-downloading.
#[derive(Default)]
pub(super) struct BlockProcessingQueue {
    ready_to_process: SingleBlockAccountSet,
    processing: SingleBlockAccountSet,
    block_cache: FxHashMap<Account, VecDeque<Block>>,
    cached_block_count: usize,
}

impl BlockProcessingQueue {
    /// Stores downloaded blocks and marks the account as ready to process.
    /// Returns the first block hash, or `None` if `blocks` is empty.
    pub fn enqueue(&mut self, account: Account, blocks: VecDeque<Block>) -> Option<BlockHash> {
        debug_assert!(!self.block_cache.contains_key(&account));
        let first_hash = blocks.front().map(|b| b.hash())?;
        self.cached_block_count += blocks.len();
        self.block_cache.insert(account, blocks);
        self.ready_to_process.insert(account, first_hash);
        Some(first_hash)
    }

    /// Removes the account from the active tracking sets (ready_to_process / processing)
    /// while keeping its blocks in the cache. Used when blocking an account that already
    /// has cached blocks, so those blocks survive until the account is unblocked.
    pub fn suspend(&mut self, account: &Account) {
        if let Some(hash) = self.first_block_hash(account) {
            if self.ready_to_process.remove_block(&hash).is_none() {
                self.processing.remove_block(&hash);
            }
        }
    }

    /// Re-inserts the account into ready_to_process using its cached blocks.
    /// Returns the first block hash, or `None` if the account has no cached blocks.
    pub fn resume(&mut self, account: Account) -> Option<BlockHash> {
        let hash = self.first_block_hash(&account)?;
        self.ready_to_process.insert(account, hash);
        Some(hash)
    }

    pub fn next_block(&self) -> Option<&Block> {
        let account = self.ready_to_process.next_account()?;
        self.block_cache.get(&account)?.front()
    }

    pub fn first_block_hash(&self, account: &Account) -> Option<BlockHash> {
        self.block_cache.get(account)?.front().map(|b| b.hash())
    }

    /// Moves the block from ready_to_process into processing. Returns the owning account.
    pub fn processing_started(&mut self, block_hash: &BlockHash) -> Option<Account> {
        let account = self.ready_to_process.remove_block(block_hash)?;
        self.processing.insert(account, *block_hash);
        Some(account)
    }

    pub fn processing_finished(&mut self, block_hash: &BlockHash) -> Option<ProcessingFinished> {
        let account = self.processing.remove_block(block_hash)?;
        self.cached_block_count -= 1;
        let next_block_hash = {
            let blocks = self.block_cache.get_mut(&account).unwrap();
            let first_block = blocks.pop_front().unwrap();
            debug_assert_eq!(first_block.hash(), *block_hash);
            blocks.front().map(|b| b.hash())
        };
        if let Some(hash) = next_block_hash {
            self.ready_to_process.insert(account, hash);
        } else {
            self.block_cache.remove(&account);
        }
        Some(ProcessingFinished {
            account,
            next_block_hash,
        })
    }

    pub fn reprocess(&mut self, account: &Account, block_hash: &BlockHash) -> bool {
        if self.processing.remove_block(block_hash).is_none() {
            return false;
        }
        self.ready_to_process.insert(*account, *block_hash);
        true
    }

    /// Removes all blocks for the account from all internal structures.
    /// Returns the number of removed (discarded) blocks.
    pub fn remove(&mut self, account: &Account) -> usize {
        self.suspend(account);
        let count = self
            .block_cache
            .remove(account)
            .map(|b| b.len())
            .unwrap_or(0);
        self.cached_block_count -= count;
        count
    }

    pub fn ready_to_process_len(&self) -> usize {
        self.ready_to_process.len()
    }

    pub fn processing_len(&self) -> usize {
        self.processing.len()
    }

    pub fn cached_block_count(&self) -> usize {
        self.cached_block_count
    }
}
