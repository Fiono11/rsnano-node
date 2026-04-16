use rsnano_node::bootstrap::bootstrapper::{Bootstrapper, logic::BootstrapQueueSnapshot};
use rsnano_types::Account;

#[derive(Default)]
pub(crate) struct BootstrapInfo {
    pub snapshot: BootstrapQueueSnapshot,
    pub search: String,
    pub add_account: String,
}

impl BootstrapInfo {
    pub(crate) fn update(&mut self, bootstrapper: &Bootstrapper) {
        let target_account = Account::parse(&self.search);
        self.snapshot = bootstrapper.queue_snapshot(50, target_account);
    }
}
