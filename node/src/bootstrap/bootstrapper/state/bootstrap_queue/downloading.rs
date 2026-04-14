use crate::bootstrap::bootstrapper::state::BootstrappingAccount;
use rsnano_types::Account;

#[derive(Default)]
pub(crate) struct DownloadingAccounts {}

impl DownloadingAccounts {
    pub fn remove(&mut self, account: &Account) -> Option<BootstrappingAccount> {
        None
    }

    pub fn len(&self) -> usize {
        0
    }

    pub fn contains(&self, account: &Account) -> bool {
        false
    }
}
