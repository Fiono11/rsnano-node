use rsnano_types::Account;
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};
use rustc_hash::FxHashSet;

/// Accounts that are currently being downloaded
#[derive(Default)]
pub(crate) struct DownloadingAccounts {
    accounts: FxHashSet<Account>,
}

impl DownloadingAccounts {
    pub fn insert(&mut self, account: Account) -> bool {
        self.accounts.insert(account)
    }

    pub fn remove(&mut self, account: &Account) -> bool {
        self.accounts.remove(account)
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn iter_accounts(&self) -> impl Iterator<Item = &Account> {
        self.accounts.iter()
    }
}

impl ContainerInfoProvider for DownloadingAccounts {
    fn container_info(&self) -> ContainerInfo {
        [("accounts", self.accounts.len(), 0)].into()
    }
}
