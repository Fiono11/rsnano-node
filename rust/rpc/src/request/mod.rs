mod node;
mod wallet;

pub(crate) use node::*;
use serde::Deserialize;
pub(crate) use wallet::*;

#[derive(Deserialize)]
pub(crate) enum RpcRequest {
    Node(NodeRpcRequest),
    Wallet(WalletRpcRequest),
    #[serde(other)]
    UnknownCommand,
}
