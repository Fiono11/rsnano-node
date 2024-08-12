use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "action")]
#[serde(rename_all = "snake_case")]
pub(crate) enum WalletRpcRequest {
    AccountCreate {
        wallet: String,
        index: Option<u32>,
    },
    AccountList {
        wallet: String,
    },
    AccountRemove {
        wallet: String,
        account: String,
    },
    #[serde(other)]
    UnknownCommand,
}
