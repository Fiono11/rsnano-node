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
        work: Option<bool>,
    },
    AccountMove {
        wallet: String,
        source: String,
        accounts: Vec<String>,
    },
    WalletAdd {
        wallet: String,
        key: String,
        work: Option<bool>,
    },
    WalletBalances {
        wallet: String,
        threshold: Option<u128>,
    },
    WalletCreate {
        seed: Option<String>,
    },
    WalletDestroy {
        wallet: String,
    },
    WalletContains {
        wallet: String,
        account: String,
    },
    #[serde(other)]
    UnknownCommand,
}
