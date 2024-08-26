use rsnano_core::{Account, WalletId};
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AccountRepresentativeSet {
    wallet: WalletId,
    account: Account,
    representative: Account,
    work: Option<u64>,
}
