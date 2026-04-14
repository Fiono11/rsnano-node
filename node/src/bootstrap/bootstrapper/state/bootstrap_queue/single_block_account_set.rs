use crate::bootstrap::bootstrapper::state::BootstrappingAccount;
use rsnano_types::{Account, Blake2Hash, BlockHash};
use rustc_hash::FxHashMap;

/// A set of accounts and one block per account
#[derive(Default)]
pub(crate) struct SingleBlockAccountSet {
    by_hash: FxHashMap<BlockHash, BootstrappingAccount>,
    by_account: FxHashMap<Account, BlockHash>,
}

impl SingleBlockAccountSet {
    pub fn insert(&mut self, entry: BootstrappingAccount) -> Option<BootstrappingAccount> {
        let first_hash = entry.blocks.front().unwrap().hash();

        let old = if let Some(old) = self.by_account.insert(entry.account, first_hash) {
            self.by_hash.remove(&old)
        } else {
            None
        };
        self.by_hash.insert(first_hash, entry);
        old
    }

    pub fn remove_block(&mut self, block_hash: &Blake2Hash) -> Option<BootstrappingAccount> {
        let entry = self.by_hash.remove(block_hash)?;
        self.by_account.remove(&entry.account);
        Some(entry)
    }

    pub fn remove_account(&mut self, account: &Account) -> Option<BootstrappingAccount> {
        let hash = self.by_account.remove(account)?;
        self.by_hash.remove(&hash)
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    pub fn len(&self) -> usize {
        self.by_account.len()
    }
}
