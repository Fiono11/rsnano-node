use rsnano_node::bootstrap::bootstrapper::state::{BootstrapLogic, BootstrappingAccountInfo};
use rsnano_types::Account;

#[derive(Default)]
pub(crate) struct BootstrapInfo {
    pub download_queue_len: usize,
    pub downloading_count: usize,
    pub ready_to_process_count: usize,
    pub blocked_accounts: usize,
    pub unblocked_accounts: usize,
    pub process_queue: usize,
    pub processing: usize,
    pub unique_blocked_accounts: usize,
    pub unknown_dependencies: usize,
    pub download_queue: Vec<BootstrappingAccountInfo>,
    pub blocked: Vec<BootstrappingAccountInfo>,
    pub search: String,
    pub add_account: String,
}

impl BootstrapInfo {
    pub(crate) fn update(&mut self, state: &BootstrapLogic) {
        let target_account = Account::parse(&self.search);
        let queue = &state.bootstrap_queue;
        self.download_queue_len = queue.download_queue_len();
        self.downloading_count = queue.downloading_count();
        self.ready_to_process_count = queue.ready_to_process_count();
        self.blocked_accounts = queue.blocked_count();
        self.unblocked_accounts = queue.unblocked_count();
        self.process_queue = queue.ready_to_process_count();
        self.processing = queue.processing_count();
        self.unique_blocked_accounts = queue.unique_blocked_accounts();
        self.unknown_dependencies = self
            .blocked_accounts
            .saturating_sub(queue.known_dependencies());

        let snapshot = queue.snapshot(50, target_account);
        self.download_queue = snapshot.download_queue;
        self.blocked = snapshot.blocked;
    }
}
