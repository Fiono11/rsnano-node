mod common;
mod ledger;
mod node;
mod utils;
mod wallets;

pub use common::*;
pub use ledger::*;
pub use node::*;
use serde::{Deserialize, Serialize};
pub use utils::*;
pub use wallets::*;

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RpcCommand {
    AccountInfo(AccountInfoArgs),
    Keepalive(AddressWithPortArg),
    Stop,
    KeyCreate,
    Receive(ReceiveArgs),
    Send(SendArgs),
    WalletAdd(WalletAddArgs),
    AccountCreate(AccountCreateArgs),
    AccountBalance(AccountBalanceArgs),
    AccountsCreate(AccountsCreateArgs),
    AccountRemove(WalletWithAccountArgs),
    AccountMove(AccountMoveArgs),
    AccountList(WalletRpcMessage),
    WalletCreate(WalletCreateArgs),
    WalletContains(WalletWithAccountArgs),
    WalletDestroy(WalletRpcMessage),
    WalletLock(WalletRpcMessage),
    WalletLocked(WalletRpcMessage),
    AccountBlockCount(AccountRpcMessage),
    AccountKey(AccountRpcMessage),
    AccountGet(KeyRpcMessage),
    AccountRepresentative(AccountRpcMessage),
    AccountWeight(AccountRpcMessage),
    AvailableSupply,
    BlockAccount(BlockHashRpcMessage),
    BlockConfirm(BlockHashRpcMessage),
    BlockCount,
    Uptime,
    FrontierCount,
    ValidateAccountNumber(AccountRpcMessage),
    NanoToRaw(AmountDto),
    RawToNano(AmountDto),
    WalletAddWatch(WalletAddWatchArgs),
    WalletRepresentative(WalletRpcMessage),
    WorkSet(WorkSetArgs),
    WorkGet(WalletWithAccountArgs),
    WalletWorkGet(WalletRpcMessage),
    AccountsFrontiers(AccountsRpcMessage),
    WalletFrontiers(WalletRpcMessage),
    Frontiers(AccountWithCountArgs),
    WalletInfo(WalletRpcMessage),
    WalletExport(WalletRpcMessage),
    PasswordChange(WalletWithPasswordArgs),
    PasswordEnter(WalletWithPasswordArgs),
    PasswordValid(WalletRpcMessage),
    DeterministicKey(DeterministicKeyArgs),
    KeyExpand(KeyExpandArgs),
    Peers(PeersArgs),
    PopulateBacklog,
    Representatives(RepresentativesArgs),
    AccountsRepresentatives(AccountsRpcMessage),
    StatsClear,
    UncheckedClear,
    Unopened(UnopenedArgs),
    NodeId,
    SearchReceivableAll,
    ReceiveMinimum,
    WalletChangeSeed(WalletChangeSeedArgs),
    Delegators(DelegatorsArgs),
    DelegatorsCount(AccountRpcMessage),
    BlockHash(BlockHashArgs),
    AccountsBalances(AccountsBalancesArgs),
    BlockInfo(BlockHashRpcMessage),
    Blocks(BlocksHashesRpcMessage),
    BlocksInfo(BlocksHashesRpcMessage),
    Chain(ChainArgs),
    Successors(ChainArgs),
    ConfirmationActive(ConfirmationActiveArgs),
    ConfirmationQuorum(ConfirmationQuorumArgs),
    WorkValidate(WorkValidateArgs),
    AccountHistory(AccountHistoryArgs),
    Sign(SignArgs),
    Process(ProcessArgs),
    WorkCancel(BlockHashRpcMessage),
    Bootstrap(BootstrapArgs),
    BootstrapAny(BootstrapAnyArgs),
    BoostrapLazy(BootsrapLazyArgs),
    WalletReceivable(WalletReceivableArgs),
    WalletRepresentativeSet(WalletRepresentativeSetArgs),
    SearchReceivable(WalletRpcMessage),
    WalletRepublish(WalletWithCountArgs),
    WalletBalances(WalletBalancesArgs),
    WalletHistory(WalletHistoryArgs),
    WalletLedger(WalletLedgerArgs),
    AccountsReceivable(AccountsReceivableArgs),
    Receivable(ReceivableArgs),
    ReceivableExists(ReceivableExistsArgs),
    RepresentativesOnline(RepresentativesOnlineArgs),
    Unchecked(UncheckedArgs),
    UncheckedGet(BlockHashRpcMessage),
    UncheckedKeys(UncheckedKeysArgs),
    ConfirmationInfo(ConfirmationInfoArgs),
    Ledger(LedgerArgs),
    WorkGenerate(WorkGenerateArgs),
    Republish(RepublishArgs),
    BlockCreate(BlockCreateArgs),
}
