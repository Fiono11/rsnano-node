use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "action")]
#[serde(rename_all = "snake_case")]
pub(crate) enum WalletRpcRequest {
    AccountCreate {
        wallet: String,
        index: Option<String>,
    },
    AccountsCreate {
        wallet: String,
        count: String,
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
        work: Option<String>,
    },
    AccountMove {
        wallet: String,
        source: String,
        accounts: Vec<String>,
    },
    WalletAdd {
        wallet: String,
        key: String,
        work: Option<String>,
    },
    WalletBalances {
        wallet: String,
        threshold: Option<String>,
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
    WalletLock {
        wallet: String,
    },
    WalletLocked {
        wallet: String,
    },
    #[serde(other)]
    UnknownCommand,
}
