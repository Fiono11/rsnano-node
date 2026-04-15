use std::collections::{BTreeMap, VecDeque};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, BlockHash};

use rustc_hash::FxHashMap;

/// A blocked account is an account that has failed to insert a new block because the source block is not currently present in the ledger
/// An account is unblocked once it has a block successfully inserted
#[derive(Default)]
pub(super) struct BlockedAccounts {
    sequenced: VecDeque<Account>,
    /// account => dep block + dep account
    by_account: FxHashMap<Account, (BlockHash, Option<Account>)>,
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
        self.sequenced.push_back(account);
        let old = self.by_account.insert(account, (dependency, None));
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
            let dep_block = self.by_account.get(account).unwrap();
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
    pub fn remove_oldest(&mut self) -> usize {
        let Some(oldest) = self.sequenced.front().cloned() else {
            return 0;
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
                removed += self.remove_account_and_dependents(account);
            }
        }

        removed
    }

    fn remove_account_and_dependents(&mut self, account: &Account) -> usize {
        let mut stack = vec![*account];
        let mut removed = 0;

        while let Some(a) = stack.pop() {
            let dep_block = self.by_account.remove(account).unwrap();
            if let Some(entry) = self.remove_account(&a) {
                removed += 1;

                if let Some(dependents) = self.by_dependency_account.get(&entry.account) {
                    stack.extend_from_slice(dependents);
                }
            }
        }

        removed
    }

    pub fn remove_account(&mut self, account: &Account) {
        let Some(dep_block) = self.by_account.remove(account) else {
            return;
        };
        self.sequenced.retain(|i| i != account);
        let accounts = self.by_dependency.get_mut(&dep_block).unwrap();
        if accounts.len() > 1 {
            accounts.retain(|i| i != account);
        } else {
            self.by_dependency.remove(&dep_block);
        }

        let dep_account = blocked.dependency_account.unwrap_or_default();
        let accounts = self.by_dependency_account.get_mut(&dep_account).unwrap();
        if accounts.len() > 1 {
            accounts.retain(|i| *i != entry.account);
        } else {
            self.by_dependency_account.remove(&dep_account);
        }
        let accounts = self.by_timestamp.get_mut(&blocked.blocked_at).unwrap();
        if accounts.len() > 1 {
            accounts.retain(|i| *i != entry.account);
        } else {
            self.by_timestamp.remove(&blocked.blocked_at);
        }
        Some(entry)
    }

    pub fn modify_dependency_account(
        &mut self,
        dependency: &BlockHash,
        new_dependency_account: Account,
    ) -> usize {
        let Some(accounts) = self.by_dependency.get(dependency) else {
            return 0;
        };

        let mut updated = 0;

        for account in accounts {
            let entry = self.by_account.get_mut(account).unwrap();
            let blocked = entry.blocked.as_mut().unwrap();
            if blocked.dependency_account != Some(new_dependency_account) {
                let old_dependency_account = blocked.dependency_account.unwrap_or_default();
                blocked.dependency_account = Some(new_dependency_account);
                let old = self
                    .by_dependency_account
                    .get_mut(&old_dependency_account)
                    .unwrap();
                if old.len() == 1 {
                    self.by_dependency_account.remove(&old_dependency_account);
                } else {
                    old.retain(|a| *a != entry.account);
                }
                self.by_dependency_account
                    .entry(new_dependency_account)
                    .or_default()
                    .push(entry.account);

                updated += 1;
            }
        }

        updated
    }

    pub fn clear(&mut self) {
        self.by_account.clear();
        self.sequenced.clear();
        self.by_dependency.clear();
        self.by_dependency_account.clear();
        self.by_timestamp.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::bootstrap::bootstrapper::state::BlockedInfo;
    use ntest::assert_false;

    #[test]
    fn empty() {
        let mut blocked = BlockedAccounts::default();
        assert_eq!(blocked.len(), 0);
        assert_eq!(blocked.is_empty(), true);
        assert_eq!(blocked.contains(&Account::from(1)), false);
        assert_eq!(blocked.count_by_dependency_account(&Account::from(1)), 0);
        assert!(blocked
            .iter_start_dep_account(Account::from(1))
            .next()
            .is_none());
        assert!(blocked.next(|_| true).is_none());
        assert!(blocked.get(&Account::from(1)).is_none());
        assert_eq!(blocked.remove_account_and_dependents(&Account::from(1)), 0);
        assert_eq!(blocked.remove_oldest(), 0);
    }

    #[test]
    fn insert_one() {
        let mut blocked = BlockedAccounts::default();

        let entry = BootstrappingAccount::new_blocked_test_instance();
        let inserted = blocked.insert(entry.clone());

        assert_eq!(inserted, true);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked.is_empty(), false);
        assert_eq!(blocked.contains(&entry.account), true);
        assert!(blocked.get(&entry.account).is_some());
        assert_eq!(
            blocked.count_by_dependency_account(&dependency_account(&blocked, &entry.account)),
            1
        );
    }

    #[test]
    fn dont_insert_if_account_already_present() {
        let mut blocked = BlockedAccounts::default();

        let entry = BootstrappingAccount::new_blocked_test_instance();
        blocked.insert(entry.clone());

        let inserted = blocked.insert(entry.clone());

        assert_eq!(inserted, false);
        assert_eq!(blocked.len(), 1);
    }

    #[test]
    fn clear() {
        let mut blocked = BlockedAccounts::default();

        let entry = BootstrappingAccount::new_blocked_test_instance();
        blocked.insert(entry.clone());
        blocked.clear();
        assert_eq!(blocked.by_timestamp.len(), 0);
        assert_eq!(blocked.by_account.len(), 0);
        assert_eq!(blocked.by_dependency.len(), 0);
        assert_eq!(blocked.by_dependency_account.len(), 0);
    }

    #[test]
    fn next() {
        let mut blocked = BlockedAccounts::default();

        let entry = BootstrappingAccount::new_blocked_test_instance();
        blocked.insert(entry);

        assert!(blocked.next(|_| true).is_some());
    }

    #[test]
    fn next_returns_none_when_all_dependency_accounts_are_known() {
        let mut blocked = BlockedAccounts::default();
        let entry = blocked_account(1, 2, 3);

        blocked.insert(entry);

        assert!(blocked.next(|_| true).is_none());
    }

    #[test]
    fn next_with_filter() {
        let mut blocked = BlockedAccounts::default();

        blocked.insert(blocked_account(1000, 100, 0));
        blocked.insert(blocked_account(2000, 200, 0));
        blocked.insert(blocked_account(3000, 300, 0));

        assert_eq!(
            blocked.next(|dep| *dep == BlockHash::from(300)),
            Some(BlockHash::from(300))
        );
    }

    #[test]
    fn pop_front() {
        let mut blocked = BlockedAccounts::default();

        let account_a = Account::from(1000);
        let account_b = Account::from(2000);

        blocked.insert(blocked_account(account_a, 100, 0));
        blocked.insert(blocked_account(account_b, 200, 0));

        assert_eq!(blocked.remove_oldest(), 1);
        assert_false!(blocked.contains(&account_a));
        assert_eq!(blocked.remove_oldest(), 1);
        assert_false!(blocked.contains(&account_b));
        assert_eq!(blocked.remove_oldest(), 0);
    }

    #[test]
    fn modify_dependency_account() {
        let mut blocked = BlockedAccounts::default();
        let account = Account::from(1000);
        let dependency = BlockHash::from(100);
        blocked.insert(blocked_account(account, dependency, 0));

        let new_dep_account = Account::from(5000);
        let updated = blocked.modify_dependency_account(&dependency, new_dep_account);

        assert_eq!(updated, 1);
        assert_eq!(dependency_account(&blocked, &account), new_dep_account);
    }

    #[test]
    fn modify_unknown_dependency_account() {
        let mut blocked = BlockedAccounts::default();
        let updated = blocked.modify_dependency_account(&1.into(), 2.into());
        assert_eq!(updated, 0);
    }

    #[test]
    fn modify_dependency_account_with_multiple_entries() {
        let mut blocked = BlockedAccounts::default();

        let dep_account = Account::from(42);
        let dep_block_1 = BlockHash::from(100);
        let dep_block_2 = BlockHash::from(200);

        let entry1 = blocked_account(1000, dep_block_1, dep_account);
        let entry2 = blocked_account(2000, dep_block_2, dep_account);
        blocked.insert(entry1.clone());
        blocked.insert(entry2.clone());

        let new_dep_account = Account::from(5000);
        let updated = blocked.modify_dependency_account(&dep_block_1, new_dep_account);

        assert_eq!(updated, 1);
        assert_eq!(
            dependency_account(&blocked, &entry1.account),
            new_dep_account
        );
        assert_ne!(
            dependency_account(&blocked, &entry2.account),
            new_dep_account
        );
    }

    #[test]
    fn modify_dependency_account_to_current_value() {
        let mut blocked = BlockedAccounts::default();

        let dep_account = Account::from(42);
        let dep_block = BlockHash::from(100);

        let entry = blocked_account(1000, dep_block, dep_account);
        blocked.insert(entry.clone());

        let updated = blocked.modify_dependency_account(&dep_block, dep_account);

        assert_eq!(updated, 0);
        assert_eq!(dependency_account(&blocked, &entry.account), dep_account);
    }

    #[test]
    fn iter_start_dependency_account() {
        let mut container = BlockedAccounts::default();

        let entry1 = blocked_account(1, 100, 10);
        let entry2 = blocked_account(2, 200, 20);
        let entry3 = blocked_account(3, 300, 30);

        container.insert(entry1);
        container.insert(entry2.clone());
        container.insert(entry3.clone());

        let result: Vec<_> = container.iter_start_dep_account(20.into()).collect();

        assert_eq!(result, vec![&entry2, &entry3]);
    }

    #[test]
    fn remove_one_of_multiple_with_same_dependency() {
        let mut container = BlockedAccounts::default();

        let same_dependency = BlockHash::from(9999);

        let entry1 = blocked_account(1, same_dependency, 10);
        let entry2 = blocked_account(2, same_dependency, 20);
        let entry3 = blocked_account(3, 300, 30);

        container.insert(entry1.clone());
        container.insert(entry2.clone());
        container.insert(entry3.clone());

        container.remove_account_and_dependents(&entry1.account);

        assert_eq!(
            container.by_dependency.get(&same_dependency).unwrap().len(),
            1
        );
    }

    #[test]
    fn remove_account_with_all_dependents() {
        let mut container = BlockedAccounts::default();

        let account_to_remove = Account::from(1000);

        let entry_to_remove = blocked_account(account_to_remove, 50, 500);
        let dependent1 = blocked_account(2000, 200, account_to_remove);
        let dependent2 = blocked_account(3000, 300, account_to_remove);
        let unrelated = blocked_account(4000, 400, 4);

        container.insert(entry_to_remove);
        container.insert(dependent1);
        container.insert(dependent2);
        container.insert(unrelated.clone());

        let removed = container.remove_account_and_dependents(&account_to_remove);

        assert_eq!(removed, 3);
        assert_eq!(container.len(), 1);
        assert!(container.contains(&unrelated.account));
    }

    #[test]
    fn remove_multiple_levels_of_dependents() {
        let mut container = BlockedAccounts::default();

        let account_to_remove = Account::from(1000);

        let entry_to_remove = blocked_account(account_to_remove, 100, 10);
        let dependent1 = blocked_account(2000, 200, account_to_remove);
        let dependent2 = blocked_account(3000, 300, account_to_remove);
        let unrelated = blocked_account(4000, 400, 40);

        container.insert(entry_to_remove);
        container.insert(dependent1);
        container.insert(dependent2);
        container.insert(unrelated.clone());

        let removed = container.remove_account_and_dependents(&account_to_remove);

        assert_eq!(removed, 3);
        assert_eq!(container.len(), 1);
        assert!(container.contains(&unrelated.account));
    }

    #[test]
    fn remove_old_entries() {
        let mut container = BlockedAccounts::default();
        let ts = Timestamp::new_test_instance();

        let entry1 = blocked_account_with_ts(1, 10, 100, ts);
        let entry2 = blocked_account_with_ts(2, 20, 200, ts + Duration::from_secs(1));
        let entry3 = blocked_account_with_ts(3, 30, 300, ts + Duration::from_secs(2));
        let entry4 = blocked_account_with_ts(4, 40, 400, ts + Duration::from_secs(3));

        container.insert(entry1.clone());
        container.insert(entry2.clone());
        container.insert(entry3.clone());
        container.insert(entry4.clone());

        let removed = container.remove_older_than(ts + Duration::from_secs(2));

        assert_eq!(removed, 2);
        assert_eq!(container.len(), 2);
        assert!(container.contains(&entry3.account));
        assert!(container.contains(&entry4.account));
    }

    /*
     * Test helpers
     */

    fn blocked_account(
        account: impl Into<Account>,
        dependency_block: impl Into<BlockHash>,
        dependency_account: impl Into<Account>,
    ) -> BootstrappingAccount {
        blocked_account_with_ts(
            account,
            dependency_block,
            dependency_account,
            Timestamp::new_test_instance(),
        )
    }

    fn blocked_account_with_ts(
        account: impl Into<Account>,
        dependency_block: impl Into<BlockHash>,
        dependency_account: impl Into<Account>,
        ts: Timestamp,
    ) -> BootstrappingAccount {
        let dep_account = dependency_account.into();

        BootstrappingAccount {
            account: account.into(),
            blocked: Some(BlockedInfo {
                dependency_block: dependency_block.into(),
                dependency_account: if dep_account.is_zero() {
                    None
                } else {
                    Some(dep_account)
                },
                blocked_at: ts,
            }),
            ..BootstrappingAccount::new_blocked_test_instance()
        }
    }

    fn dependency_account(blocked: &BlockedAccounts, account: &Account) -> Account {
        blocked
            .get(account)
            .expect("should contain account")
            .blocked
            .as_ref()
            .expect("should have blocked info")
            .dependency_account
            .unwrap_or_default()
    }
}
