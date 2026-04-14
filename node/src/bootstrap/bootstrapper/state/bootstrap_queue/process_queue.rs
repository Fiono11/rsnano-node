use crate::bootstrap::bootstrapper::state::BootstrappingAccount;
use rsnano_types::Account;

/// Queue for downloaded blocks that are waiting to be
/// inserted into the block processor queue
#[derive(Default)]
pub(crate) struct ProcessQueue {}
impl ProcessQueue {
    pub fn remove(&self, account: &Account) -> Option<BootstrappingAccount> {
        // TODO
        None
    }
}
