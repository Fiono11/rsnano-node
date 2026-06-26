use rsnano_node::bootstrap::bootstrapper::{
    BootstrapQueueSnapshot, Bootstrapper, BootstrappingAccountInfo, PeerScoreSnapshot,
};
use rsnano_types::Account;

use crate::insight::gui::FrontierScanViewModel;

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
    FrontierScan,
}

impl BootstrapViewType {
    pub fn as_str(&self) -> &'static str {
        match self {
            BootstrapViewType::BootstrapQueue => "Bootstrap Queue",
            BootstrapViewType::PeerScores => "Peer Scores",
            BootstrapViewType::FrontierScan => "Frontier Scan",
        }
    }

    pub fn all() -> [BootstrapViewType; 3] {
        [Self::BootstrapQueue, Self::PeerScores, Self::FrontierScan]
    }
}

pub(crate) enum BootstrapDetails {
    BootstrapQueue(BootstrapQueueViewModel),
    PeerScores(PeerScoresViewModel),
    FrontierScan(FrontierScanViewModel),
}
impl BootstrapDetails {
    pub fn view_type(&self) -> BootstrapViewType {
        match self {
            BootstrapDetails::BootstrapQueue(_) => BootstrapViewType::BootstrapQueue,
            BootstrapDetails::PeerScores(_) => BootstrapViewType::PeerScores,
            BootstrapDetails::FrontierScan(_) => BootstrapViewType::FrontierScan,
        }
    }
}

#[derive(Default)]
pub(crate) struct BootstrapQueueViewModel {
    pub download_queue_len: String,
    pub blocked_accounts: String,
    pub unblocked_accounts: String,
    pub process_queue: String,
    pub processing: String,
    pub downloading_count: String,
    pub unique_blocking_accounts: usize,
    pub unknown_dependencies: usize,
    pub cached_blocks: String,
    pub discarded_blocks: String,
    pub download_queue: Vec<AccountViewModel>,
    pub downloading: Vec<AccountViewModel>,
    pub blocked: Vec<AccountViewModel>,
}

pub(crate) struct AccountViewModel {
    pub account: String,
    pub priority: String,
    pub dependency: String,
    pub dependency_account: String,
    pub account_val: Account,
    pub dependency_account_val: Account,
}

impl From<&BootstrappingAccountInfo> for AccountViewModel {
    fn from(e: &BootstrappingAccountInfo) -> Self {
        let mut account = e.account.encode_account();
        let mut dependency = e.dependency_block.to_string();
        let mut dependency_account = e.dependency_account.encode_account();
        truncate_text(&mut account, 20);
        truncate_text(&mut dependency, 15);
        truncate_text(&mut dependency_account, 20);
        Self {
            account,
            priority: format!("{:.2}", e.priority.as_f64()),
            dependency,
            dependency_account,
            account_val: e.account,
            dependency_account_val: e.dependency_account,
        }
    }
}

pub(crate) struct PeerScoresViewModel {
    pub peers: Vec<PeerScoreSnapshot>,
}

fn truncate_text(s: &mut String, len: usize) {
    if s.len() > len {
        s.replace_range(len.., "...");
    }
}
