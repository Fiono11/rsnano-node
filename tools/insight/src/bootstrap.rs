use rsnano_node::bootstrap::bootstrapper::state::{BootstrapLogic, BootstrapQueueSnapshot};
use rsnano_types::Account;

#[derive(Default)]
pub(crate) struct BootstrapInfo {
    pub snapshot: BootstrapQueueSnapshot,
    pub search: String,
    pub add_account: String,
}

impl BootstrapInfo {
    pub(crate) fn update(&mut self, state: &BootstrapLogic) {
        let target_account = Account::parse(&self.search);
        let queue = &state.bootstrap_queue;
        self.snapshot = queue.snapshot(50, target_account);
    }
}
