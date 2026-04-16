use rsnano_types::{Account, Blake2Hash, BlockHash};
use rustc_hash::FxHashMap;

/// A set of accounts and one block per account
#[derive(Default)]
pub(crate) struct SingleBlockAccountSet {
    by_hash: FxHashMap<BlockHash, Account>,
    by_account: FxHashMap<Account, BlockHash>,
}

impl SingleBlockAccountSet {
    pub fn insert(&mut self, account: Account, block_hash: BlockHash) {
        if let Some(old) = self.by_account.insert(account, block_hash) {
            self.by_hash.remove(&old);
        }

        self.by_hash.insert(block_hash, account);
    }

    pub fn next_account(&self) -> Option<Account> {
        self.by_hash.values().next().cloned()
    }

    // TODO add account arg just for safety
    pub fn remove_block(&mut self, block_hash: &Blake2Hash) -> Option<Account> {
        let account = self.by_hash.remove(block_hash)?;
        self.by_account.remove(&account);
        Some(account)
    }

    pub fn len(&self) -> usize {
        self.by_hash.len()
    }
}
