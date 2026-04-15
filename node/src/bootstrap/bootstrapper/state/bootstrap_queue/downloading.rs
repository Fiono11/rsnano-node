use crate::bootstrap::bootstrapper::state::BootstrappingAccount;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::Account;
use rustc_hash::FxHashMap;
use std::{collections::BTreeSet, time::Duration};

/// Accounts that are currently being downloaded
#[derive(Default)]
pub(crate) struct DownloadingAccounts {
    by_account: FxHashMap<Account, (BootstrappingAccount, Timestamp)>,
    by_time: BTreeSet<(Timestamp, Account)>,
}

impl DownloadingAccounts {
    pub fn insert(
        &mut self,
        entry: BootstrappingAccount,
        now: Timestamp,
    ) -> Option<BootstrappingAccount> {
        self.by_time.insert((now, entry.account));
        let (old_entry, ts) = self.by_account.insert(entry.account, (entry, now))?;
        self.by_time.remove(&(ts, old_entry.account));
        Some(old_entry)
    }
    pub fn remove(&mut self, account: &Account) {
        if let Some((entry, ts)) = self.by_account.remove(account) {
            self.by_time.remove(&(ts, entry.account));
            Some(entry)
        } else {
            None
        }
    }

    pub fn len(&self) -> usize {
        self.by_account.len()
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    pub fn pop_timeout(&mut self, now: Timestamp) -> Option<BootstrappingAccount> {
        let first = self.by_time.first()?;
        if first.0 < now - Duration::from_secs(15) {
            let account = self.by_time.pop_first().unwrap().1;
            self.by_account.remove(&account).map(|(i, _)| i)
        } else {
            None
        }
    }
}
