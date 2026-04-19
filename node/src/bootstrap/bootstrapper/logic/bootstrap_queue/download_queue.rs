use rsnano_types::Account;
use std::collections::BTreeMap;

use super::priority::{Priority, PriorityKeyDesc};
use rustc_hash::FxHashSet;

/// Queue of bootstrapping accounts that are ready to download blocks
#[derive(Default)]
pub(super) struct DownloadQueue {
    by_priority: BTreeMap<PriorityKeyDesc, FxHashSet<Account>>, // descending
    account_count: usize,
}

#[derive(PartialEq, Eq)]
pub(crate) enum ChangePriorityResult {
    Updated,
    Removed,
    NotFound,
    Unchanged,
}

impl DownloadQueue {
    pub fn len(&self) -> usize {
        self.account_count
    }

    pub fn is_empty(&self) -> bool {
        self.account_count == 0
    }

    pub fn insert(&mut self, account: Account, priority: Priority) {
        let inserted = self
            .by_priority
            .entry(priority.into())
            .or_default()
            .insert(account);
        debug_assert!(inserted);
        self.account_count += 1;
    }

    pub fn pop_lowest_prio(&mut self) -> Option<Account> {
        let mut last_entry = self.by_priority.last_entry()?;
        let accounts = last_entry.get_mut();
        let account = *accounts.iter().next().unwrap();
        if accounts.len() == 1 {
            last_entry.remove();
        } else {
            accounts.remove(&account);
        }
        self.account_count -= 1;
        Some(account)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Priority, &Account)> {
        self.by_priority
            .iter()
            .flat_map(|(prio, accs)| accs.iter().map(|a| (prio.0, a)))
    }

    pub fn remove(&mut self, account: &Account, priority: Priority) -> bool {
        let Some(ids) = self.by_priority.get_mut(&priority.into()) else {
            return false;
        };
        if !ids.remove(account) {
            return false;
        }
        if ids.is_empty() {
            self.by_priority.remove(&priority.into());
        }
        self.account_count -= 1;
        true
    }

    pub fn change_priority(
        &mut self,
        account: &Account,
        old_prio: Priority,
        new_prio: Priority,
    ) -> bool {
        let Some(accounts) = self.by_priority.get_mut(&PriorityKeyDesc(old_prio)) else {
            return false;
        };
        if !accounts.remove(account) {
            return false;
        }
        if accounts.is_empty() {
            self.by_priority.remove(&PriorityKeyDesc(old_prio));
        }
        self.by_priority
            .entry(PriorityKeyDesc(new_prio))
            .or_default()
            .insert(*account);
        true
    }
}
