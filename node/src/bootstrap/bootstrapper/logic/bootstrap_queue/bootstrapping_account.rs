use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, BlockHash};

use super::Priority;

/// An account that is currently being bootstrapped
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct BootstrappingAccount {
    pub account: Account,
    pub priority: Priority,
    pub fails: usize,
    pub last_request: Option<Timestamp>,
    pub blocked: Option<BlockedInfo>,
    pub state: AccountState,
}

impl BootstrappingAccount {
    pub fn new(account: Account, priority: Priority) -> Self {
        if account.is_zero() {
            panic!("The zero account can never be a boostrapping account!")
        }
        Self {
            account,
            priority,
            fails: 0,
            last_request: None,
            blocked: None,
            state: AccountState::EnqueuedForDownload,
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub(crate) enum AccountState {
    #[default]
    EnqueuedForDownload,
    Downloading,
    ReadyToProcess,
    Processing,
    Blocked,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct BlockedInfo {
    pub dependency_block: BlockHash,
    /// Account that contains the dependency block, fetched via a background dependency walker
    pub dependency_account: Option<Account>,
    pub blocked_at: Timestamp,
}
