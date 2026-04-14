use crate::bootstrap::bootstrapper::state::BootstrappingAccount;
use rsnano_types::Account;

#[derive(Default)]
pub(crate) struct DownloadingAccounts {}

impl DownloadingAccounts {
    pub fn remove(&mut self, account: &Account) -> Option<BootstrappingAccount> {
        None
    }
}
