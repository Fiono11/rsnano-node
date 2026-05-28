use rsnano_node::bootstrap::bootstrapper::{BootstrapQueueSnapshot, Bootstrapper};
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BootstrapViewType {
    BootstrapQueue,
    PeerScores,
}

impl BootstrapViewType {
    pub fn as_str(&self) -> &'static str {
        match self {
            BootstrapViewType::BootstrapQueue => "Bootstrap Queue",
            BootstrapViewType::PeerScores => "Peer Scores",
        }
    }

    pub fn all() -> [BootstrapViewType; 2] {
        [Self::BootstrapQueue, Self::PeerScores]
    }
}
