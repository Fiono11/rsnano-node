use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "action")]
#[serde(rename_all = "snake_case")]
pub(crate) enum RpcRequest {
    Version,
    AccountBlockCount {
        account: String,
    },
    AccountBalance {
        account: String,
        only_confirmed: Option<bool>,
    },
    AccountGet {
        key: String,
    },
    AccountKey {
        account: String,
    },
    AccountRepresentative {
        account: String,
    },
    AccountWeight {
        account: String,
    },
    AvailableSupply,
    BlockAccount {
        hash: String,
    },
    BlockConfirm {
        hash: String,
    },
    BlockCount,
    AccountCreate {
        wallet: String,
        index: Option<u32>,
    },
    #[serde(other)]
    UnknownCommand,
}
