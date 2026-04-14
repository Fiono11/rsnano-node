use rsnano_node::bootstrap::bootstrapper::state::{BootstrapLogic, Priority};
use rsnano_types::{Account, BlockHash};

#[derive(Default)]
pub(crate) struct BootstrapInfo {
    pub prioritized_accounts: usize,
    pub blocked_accounts: usize,
    pub unique_blocked_accounts: usize,
    pub unknown_dependencies: usize,
    pub priorities: Vec<(Priority, Account)>,
    pub blocked: Vec<(Account, BlockHash, Account)>,
    pub search: String,
    pub add_account: String,
}

impl BootstrapInfo {
    pub(crate) fn update(&mut self, state: &BootstrapLogic) {
        let target_account = Account::parse(&self.search);
        let queue = &state.bootstrap_queue;
        self.prioritized_accounts = queue.download_queue_len();
        self.blocked_accounts = queue.blocked_count();
        self.unique_blocked_accounts = queue.unique_blocked_accounts();
        self.unknown_dependencies = self
            .blocked_accounts
            .saturating_sub(queue.known_dependencies());

        self.priorities = queue
            .iter_priorities()
            .filter_map(|(prio, acc)| {
                if target_account.is_none() || target_account.as_ref() == Some(acc) {
                    Some((prio, *acc))
                } else {
                    None
                }
            })
            .take(50)
            .collect();

        self.blocked = queue
            .iter_blocked()
            .filter(|(account, _, dep_account)| {
                target_account.is_none()
                    || target_account == Some(*account)
                    || target_account == Some(*dep_account)
            })
            .take(50)
            .collect();
    }
}
