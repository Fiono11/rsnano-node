use rsnano_types::Account;

use super::Priority;

/// An account that is currently being bootstrapped
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct BootstrappingAccount {
    pub account: Account,
    pub priority: Priority,
}

impl BootstrappingAccount {
    pub fn new(account: Account, priority: Priority) -> Self {
        if account.is_zero() {
            panic!("The zero account can never be a boostrapping account!")
        }
        Self { account, priority }
    }
}
