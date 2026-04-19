use rsnano_nullable_clock::Timestamp;
use rsnano_types::Account;

use super::Priority;

/// An account that is currently being bootstrapped
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct BootstrappingAccount {
    pub account: Account,
    pub priority: Priority,
    pub fails: usize,
    pub last_request: Option<Timestamp>,
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
        }
    }
}
