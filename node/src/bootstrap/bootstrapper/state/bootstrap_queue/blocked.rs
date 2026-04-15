use std::collections::{BTreeMap, VecDeque};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, BlockHash};

use rustc_hash::FxHashMap;

/// A blocked account is an account that has failed to insert a new block because the source block is not currently present in the ledger
/// An account is unblocked once it has a block successfully inserted
#[derive(Default)]
pub(super) struct BlockedAccounts {
    sequenced: VecDeque<Account>,
    /// account => dep block + dep account + blocked_at
    by_account: FxHashMap<Account, (BlockHash, Option<Account>, Timestamp)>,
    by_dependency: BTreeMap<BlockHash, Vec<Account>>,
    by_dependency_account: BTreeMap<Account, Vec<Account>>,
    by_timestamp: BTreeMap<Timestamp, Vec<Account>>,
}

impl BlockedAccounts {
    pub fn len(&self) -> usize {
        self.sequenced.len()
    }

    pub(crate) fn known_dependencies(&self) -> usize {
        self.by_dependency_account
            .range(Account::from(1)..)
            .map(|(_, accs)| accs.len())
            .sum()
    }

    pub(crate) fn unique_dependency_accounts(&self) -> usize {
        let mut known = self.by_dependency_account.len();
        if self.by_dependency_account.contains_key(&Account::ZERO) {
            known -= 1;
        }
        known
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn insert(&mut self, account: Account, dependency: BlockHash, timestamp: Timestamp) {
        debug_assert!(!account.is_zero());
        self.sequenced.push_back(account);
        let old = self
            .by_account
            .insert(account, (dependency, None, timestamp));
        debug_assert!(old.is_none());
        self.by_dependency
            .entry(dependency)
            .or_default()
            .push(account);
        self.by_dependency_account
            .entry(Account::ZERO)
            .or_default()
            .push(account);
        self.by_timestamp
            .entry(timestamp)
            .or_default()
            .push(account);
    }

    pub fn count_by_dependency_account(&self, dep_account: &Account) -> usize {
        self.by_dependency_account
            .get(dep_account)
            .map(|accs| accs.len())
            .unwrap_or_default()
    }

    pub fn next(&self, filter: impl Fn(&BlockHash) -> bool) -> Option<BlockHash> {
        // Scan all entries with unknown dependency account
        let accounts = self.by_dependency_account.get(&Account::ZERO)?;
        accounts.iter().find_map(|account| {
            let (dep_block, _, _) = self.by_account.get(account).unwrap();
            if filter(dep_block) {
                Some(*dep_block)
            } else {
                None
            }
        })
    }

    pub fn iter_start_dep_account(&self, start: Account) -> impl Iterator<Item = &Account> {
        self.by_dependency_account
            .range(start..)
            .flat_map(|(_, accs)| accs)
    }

    pub fn iter_by_insertion_order(&self) -> impl Iterator<Item = &Account> {
        self.sequenced.iter()
    }

    /// Removes the oldest entry and all entries dependent on that
    /// Returns the number of removed entries
    pub fn remove_oldest(&mut self) -> Vec<Account> {
        let Some(oldest) = self.sequenced.front().cloned() else {
            return Vec::new();
        };

        self.remove_account_and_dependents(&oldest)
    }

    /// Removes entries older than the given cutoff and all entries dependent on them
    /// Returns the number of removed entries
    pub fn remove_older_than(&mut self, cutoff: Timestamp) -> Vec<Account> {
        let mut removed = Vec::new();
        while let Some((timestamp, accounts)) = self.by_timestamp.first_key_value() {
            if *timestamp >= cutoff {
                // Entries are sorted by timestamp, no need to continue
                break;
            }
            let accounts = accounts.clone();
            for account in &accounts {
                removed.extend(self.remove_account_and_dependents(account));
            }
        }

        removed
    }

    pub fn remove_account_and_dependents(&mut self, account: &Account) -> Vec<Account> {
        let mut stack = vec![*account];
        let mut removed = Vec::new();

        while let Some(account) = stack.pop() {
            if !self.remove(&account) {
                continue;
            }

            removed.push(account);

            if let Some(dependents) = self.by_dependency_account.get(&account) {
                stack.extend_from_slice(dependents);
            }
        }

        removed
    }

    pub fn remove(&mut self, account: &Account) -> bool {
        let Some((dep_block, dep_account, blocked_at)) = self.by_account.remove(account) else {
            return false;
        };

        self.sequenced.retain(|i| i != account);
        let dep_accounts = self.by_dependency.get_mut(&dep_block).unwrap();
        if dep_accounts.len() > 1 {
            dep_accounts.retain(|i| i != account);
        } else {
            self.by_dependency.remove(&dep_block);
        }

        let dep_account = dep_account.unwrap_or_default();
        let dep_accounts = self.by_dependency_account.get_mut(&dep_account).unwrap();
        if dep_accounts.len() > 1 {
            dep_accounts.retain(|i| i != account);
        } else {
            self.by_dependency_account.remove(&dep_account);
        }

        let accounts = self.by_timestamp.get_mut(&blocked_at).unwrap();
        if accounts.len() > 1 {
            accounts.retain(|i| i != account);
        } else {
            self.by_timestamp.remove(&blocked_at);
        }

        true
    }

    pub fn modify_dependency_account(
        &mut self,
        dependency: &BlockHash,
        new_dependency_account: Account,
    ) -> Vec<Account> {
        let mut updated = Vec::new();
        let Some(accounts) = self.by_dependency.get(dependency) else {
            return updated;
        };

        for account in accounts {
            let (_, dep_account, _) = self.by_account.get_mut(account).unwrap();
            if *dep_account != Some(new_dependency_account) {
                let old_dependency_account = dep_account.unwrap_or_default();
                *dep_account = Some(new_dependency_account);
                let old = self
                    .by_dependency_account
                    .get_mut(&old_dependency_account)
                    .unwrap();
                if old.len() == 1 {
                    self.by_dependency_account.remove(&old_dependency_account);
                } else {
                    old.retain(|a| a != account);
                }
                self.by_dependency_account
                    .entry(new_dependency_account)
                    .or_default()
                    .push(*account);

                updated.push(*account);
            }
        }

        updated
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    pub fn clear(&mut self) {
        self.by_account.clear();
        self.sequenced.clear();
        self.by_dependency.clear();
        self.by_dependency_account.clear();
        self.by_timestamp.clear();
    }
}
