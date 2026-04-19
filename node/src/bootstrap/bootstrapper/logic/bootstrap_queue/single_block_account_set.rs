use rsnano_types::{Account, Blake2Hash, BlockHash};
use rustc_hash::FxHashMap;

/// A set of accounts and one block per account
#[derive(Default)]
pub(crate) struct SingleBlockAccountSet {
    by_hash: FxHashMap<BlockHash, Account>,
}

impl SingleBlockAccountSet {
    pub fn insert(&mut self, account: Account, block_hash: BlockHash) {
        self.by_hash.insert(block_hash, account);
    }

    pub fn next_account(&self) -> Option<Account> {
        self.by_hash.values().next().cloned()
    }

    // TODO add account arg just for safety
    pub fn remove_block(&mut self, block_hash: &Blake2Hash) -> Option<Account> {
        self.by_hash.remove(block_hash)
    }

    pub fn len(&self) -> usize {
        self.by_hash.len()
    }
}
