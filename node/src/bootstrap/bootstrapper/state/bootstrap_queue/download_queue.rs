use rsnano_nullable_clock::Timestamp;
use rsnano_types::Account;
use std::collections::BTreeMap;
use std::mem::size_of;

use rustc_hash::FxHashSet;

use super::priority::{Priority, PriorityKeyDesc};
use crate::bootstrap::bootstrapper::state::BootstrappingAccount;

/// Queue of bootstrapping accounts that are ready to download blocks
#[derive(Default)]
pub(super) struct DownloadQueue {
    by_account: BTreeMap<Account, BootstrappingAccount>,
    by_priority: BTreeMap<PriorityKeyDesc, FxHashSet<Account>>, // descending
}

pub(crate) enum ChangePriorityResult {
    Updated,
    Deleted,
    NotFound,
}

impl DownloadQueue {
    pub const ELEMENT_SIZE: usize = size_of::<BootstrappingAccount>()
        + size_of::<Account>()
        + size_of::<f32>()
        + size_of::<u64>() * 4;

    pub fn len(&self) -> usize {
        self.by_account.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_account.is_empty()
    }

    pub fn get(&self, account: &Account) -> Option<&BootstrappingAccount> {
        self.by_account.get(account)
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    pub fn insert(&mut self, entry: BootstrappingAccount) -> bool {
        let account = entry.account;
        let priority = entry.priority;

        if self.by_account.contains_key(&account) {
            return false;
        }

        self.by_account.insert(account, entry);
        self.by_priority
            .entry(priority.into())
            .or_default()
            .insert(account);
        true
    }

    pub fn pop_lowest_prio(&mut self) -> Option<BootstrappingAccount> {
        let lowest_prio_account = {
            let (_, v) = self.by_priority.last_key_value()?;
            *v.iter().next().unwrap()
        };
        Some(self.remove_account(&lowest_prio_account))
    }

    pub fn set_last_request(&mut self, account: &Account, timestamp: Option<Timestamp>) {
        if let Some(entry) = self.by_account.get_mut(account) {
            entry.last_request = timestamp;
        }
    }

    pub fn modify(
        &mut self,
        account: &Account,
        f: impl Fn(&mut BootstrappingAccount),
    ) ->  Option<&BootstrappingAccount>{
        let entry = self.by_account.get_mut(account)?;
            let old_prio = entry.priority;
            f(entry);
            if entry.priority != old_prio {
                let new_prio = entry.priority;
                self.change_priority_internal(account, old_prio, new_prio)
            }

        Some(entry)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Priority, &Account)> {
        self.by_priority
            .iter()
            .flat_map(|(prio, accs)| accs.iter().map(|a| (prio.0, a)))
    }

    pub fn next_priority(
        &self,
        cutoff: Timestamp,
        filter: impl Fn(&Account) -> bool,
    ) -> Option<&BootstrappingAccount> {
        self.by_priority
            .values()
            .flatten()
            .map(|account| self.by_account.get(account).unwrap())
            .find(|entry| {
                if let Some(ts) = entry.last_request
                    && ts > cutoff
                {
                    return false;
                }
                filter(&entry.account)
            })
    }

    pub fn remove(&mut self, account: &Account) -> Option<BootstrappingAccount> {
        if let Some(entry) = self.by_account.remove(account) {
            self.remove_priority(account, entry.priority);
            Some(entry)
        } else {
            None
        }
    }

    fn change_priority_internal(
        &mut self,
        account: &Account,
        old_prio: Priority,
        new_prio: Priority,
    ) {
        self.remove_priority(account, old_prio);
        self.by_priority
            .entry(new_prio.into())
            .or_default()
            .insert(*account);
    }

    fn remove_account(&mut self, account: &Account) -> BootstrappingAccount {
        let entry = self.by_account.remove(account).unwrap();
        self.remove_priority(account, entry.priority);
        entry
    }

    fn remove_priority(&mut self, account: &Account, priority: Priority) {
        let ids = self.by_priority.get_mut(&priority.into()).unwrap();
        if ids.len() > 1 {
            ids.remove(account);
        } else {
            self.by_priority.remove(&priority.into());
        }
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.by_account.clear();
        self.by_priority.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let mut queue = DownloadQueue::default();
        assert_eq!(queue.len(), 0);
        assert!(queue.is_empty());
        assert!(queue.get(&Account::from(1)).is_none());
        assert_eq!(queue.contains(&Account::from(1)), false);
        assert!(queue.pop_lowest_prio().is_none());
        assert!(queue.remove(&Account::from(1)).is_none());
    }

    #[test]
    fn insert_one() {
        let mut queue = DownloadQueue::default();
        let entry = BootstrappingAccount::new_test_instance();
        assert!(queue.insert(entry.clone()));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.is_empty(), false);
        assert_eq!(queue.contains(&entry.account), true);
        assert!(queue.get(&entry.account).is_some());
    }

    #[test]
    fn insert_two() {
        let mut queue = DownloadQueue::default();
        assert!(queue.insert(BootstrappingAccount::new(
            Account::from(1),
            Priority::new(2.5)
        )));
        assert!(queue.insert(BootstrappingAccount::new(
            Account::from(2),
            Priority::new(3.5)
        )));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.is_empty(), false);
        assert_eq!(queue.contains(&Account::from(1)), true);
        assert_eq!(queue.contains(&Account::from(2)), true);
    }

    #[test]
    fn dont_insert_when_account_already_present() {
        let mut queue = DownloadQueue::default();
        queue.insert(BootstrappingAccount::new(
            Account::from(1),
            Priority::new(2.5),
        ));
        let inserted = queue.insert(BootstrappingAccount::new(
            Account::from(1),
            Priority::new(3.5),
        ));
        assert_eq!(inserted, false);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn pop_front() {
        let mut queue = DownloadQueue::default();
        queue.insert(BootstrappingAccount::new(
            Account::from(1),
            Priority::new(2.5),
        ));
        queue.insert(BootstrappingAccount::new(
            Account::from(2),
            Priority::new(2.5),
        ));
        queue.insert(BootstrappingAccount::new(
            Account::from(3),
            Priority::new(2.5),
        ));

        assert_eq!(queue.pop_lowest_prio().unwrap().account, Account::from(1));
        assert_eq!(queue.pop_lowest_prio().unwrap().account, Account::from(2));
        assert_eq!(queue.pop_lowest_prio().unwrap().account, Account::from(3));
        assert!(queue.pop_lowest_prio().is_none());
    }

    #[test]
    fn change_timestamp() {
        let account = Account::from(1);
        let mut queue = DownloadQueue::default();
        queue.insert(BootstrappingAccount::new(account, Priority::new(2.5)));
        let now = Timestamp::new_test_instance();

        queue.set_last_request(&account, Some(now));

        assert_eq!(queue.get(&account).unwrap().last_request, Some(now));
    }

    mod next_priority {
        use super::*;
        use std::time::Duration;

        #[test]
        fn empty() {
            let queue = DownloadQueue::default();
            let next = queue.next_priority(Timestamp::new_test_instance(), |_account| true);
            assert!(next.is_none());
        }

        #[test]
        fn one_item() {
            let mut queue = DownloadQueue::default();
            let account = Account::from(1);
            queue.insert(BootstrappingAccount::new(account, Priority::new(2.5)));

            let next = queue
                .next_priority(Timestamp::new_test_instance(), |_account| true)
                .unwrap();

            assert_eq!(next.account, account);
        }

        #[test]
        fn ordered_by_priority_desc() {
            let mut queue = DownloadQueue::default();
            queue.insert(BootstrappingAccount::new(
                Account::from(1),
                Priority::new(2.5),
            ));
            queue.insert(BootstrappingAccount::new(
                Account::from(2),
                Priority::new(10.0),
            ));
            queue.insert(BootstrappingAccount::new(
                Account::from(3),
                Priority::new(3.5),
            ));

            let next = queue
                .next_priority(Timestamp::new_test_instance(), |_account| true)
                .unwrap();

            assert_eq!(next.account, Account::from(2));
        }

        #[test]
        fn cutoff() {
            let now = Timestamp::new_test_instance();
            let a = BootstrappingAccount::new(Account::from(1), Priority::new(2.5));
            let mut b = BootstrappingAccount::new(Account::from(2), Priority::new(10.0));
            b.last_request = Some(now);
            let mut c = BootstrappingAccount::new(Account::from(3), Priority::new(3.5));
            c.last_request = Some(now - Duration::from_mins(1));
            let mut queue = DownloadQueue::default();
            queue.insert(a);
            queue.insert(b);
            queue.insert(c);

            let next = queue
                .next_priority(now - Duration::from_secs(30), |_account| true)
                .unwrap();

            assert_eq!(next.account, Account::from(3));
        }

        #[test]
        fn filter() {
            let a = BootstrappingAccount::new(Account::from(1), Priority::new(2.5));
            let b = BootstrappingAccount::new(Account::from(2), Priority::new(10.0));
            let c = BootstrappingAccount::new(Account::from(3), Priority::new(3.5));
            let mut queue = DownloadQueue::default();
            queue.insert(a);
            queue.insert(b);
            queue.insert(c);

            let next = queue
                .next_priority(Timestamp::new_test_instance(), |account| {
                    *account == Account::from(1)
                })
                .unwrap();

            assert_eq!(next.account, Account::from(1));
        }
    }

    #[test]
    fn change_priority() {
        let mut queue = DownloadQueue::default();
        queue.insert(BootstrappingAccount::new(
            Account::from(1),
            Priority::new(2.5),
        ));
        queue.insert(BootstrappingAccount::new(
            Account::from(2),
            Priority::new(3.0),
        ));
        queue.insert(BootstrappingAccount::new(
            Account::from(3),
            Priority::new(3.5),
        ));

        let mut old_priority = Priority::ZERO;
        let new_priority = Priority::new(10.0);

        queue.modify(&Account::from(2), |entry| {
            old_priority = entry.priority;
            entry.priority = new_priority;
            true
        });

        assert_eq!(old_priority, Priority::new(3.0));
        assert_eq!(queue.get(&Account::from(2)).unwrap().priority, new_priority);

        let next = queue
            .next_priority(Timestamp::new_test_instance(), |_| true)
            .unwrap();
        assert_eq!(next.account, Account::from(2));
    }

    #[test]
    fn remove_by_priority_change() {
        let mut queue = DownloadQueue::default();
        let account = Account::from(1);
        queue.insert(BootstrappingAccount::new(account, Priority::new(2.5)));

        queue.modify(&account, |_| false);

        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn remove() {
        let mut queue = DownloadQueue::default();
        queue.insert(BootstrappingAccount::new(
            Account::from(1),
            Priority::new(2.5),
        ));
        queue.insert(BootstrappingAccount::new(
            Account::from(2),
            Priority::new(3.0),
        ));
        queue.insert(BootstrappingAccount::new(
            Account::from(3),
            Priority::new(3.5),
        ));

        let removed = queue.remove(&Account::from(2)).unwrap();

        assert_eq!(removed.account, Account::from(2));
        assert_eq!(queue.len(), 2);
    }
}
