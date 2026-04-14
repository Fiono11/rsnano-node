use rsnano_types::Account;

pub(crate) struct BlockInfo {
    pub account: Option<Account>,
    pub was_last: bool,
}
