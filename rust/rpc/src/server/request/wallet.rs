use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "action")]
#[serde(rename_all = "snake_case")]
pub(crate) enum WalletRpcRequest {
    AccountCreate {
        wallet: String,
        index: Option<u32>,
    },
    AccountsCreate {
        wallet: String,
        count: u32,
    },
    AccountList {
        wallet: String,
    },
    AccountRemove {
        wallet: String,
        account: String,
    },
    AccountRepresentativeSet {
        wallet: String,
        account: String,
        representative: String,
    },
    #[serde(other)]
    UnknownCommand,
}
