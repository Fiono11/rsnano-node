use rsnano_nullable_clock::Timestamp;
use rsnano_types::Account;
use rustc_hash::FxHashMap;
use std::{collections::BTreeSet, time::Duration};

/// Accounts that are currently being downloaded
#[derive(Default)]
pub(crate) struct DownloadingAccounts {
    by_account: FxHashMap<Account, Timestamp>,
    by_time: BTreeSet<(Timestamp, Account)>,
}

impl DownloadingAccounts {
    pub fn insert(&mut self, account: Account, now: Timestamp) -> Option<Timestamp> {
        let old = self.by_account.insert(account, now);
        if let Some(ts) = old {
            self.by_time.remove(&(ts, account));
        }
        self.by_time.insert((now, account));
        old
    }

    pub fn remove(&mut self, account: &Account) -> bool {
        if let Some(ts) = self.by_account.remove(account) {
            self.by_time.remove(&(ts, *account));
            true
        } else {
            false
        }
    }

    pub fn len(&self) -> usize {
        self.by_account.len()
    }

    pub fn pop_timeout(&mut self, now: Timestamp) -> Option<Account> {
        let first = self.by_time.first()?;
        if first.0 < now - Duration::from_secs(15) {
            let account = self.by_time.pop_first().unwrap().1;
            self.by_account.remove(&account);
            Some(account)
        } else {
            None
        }
    }
}
