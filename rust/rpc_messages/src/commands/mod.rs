mod ledger;
mod node;
mod utils;
mod wallets;

pub use ledger::*;
pub use node::*;
use serde::{Deserialize, Serialize};
pub use utils::*;
pub use wallets::*;

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RpcCommand {
    AccountInfo(AccountInfoArgs),
    Keepalive(KeepaliveArgs),
    Stop,
    KeyCreate,
    Receive(ReceiveArgs),
    Send(SendArgs),
    WalletAdd(WalletAddArgs),
    WalletCreate,
    Version,
}

#[cfg(test)]
mod tests {
    use crate::RpcCommand;
    use serde_json::{from_str, to_string_pretty};

    #[test]
    fn serialize_stop_command() {
        assert_eq!(
            to_string_pretty(&RpcCommand::Stop).unwrap(),
            r#"{
  "action": "stop"
}"#
        )
    }

    #[test]
    fn serialize_version_command() {
        assert_eq!(
            to_string_pretty(&RpcCommand::Version).unwrap(),
            r#"{
  "action": "version"
}"#
        )
    }

    #[test]
    fn deserialize_version_command() {
        let json = r#"{"action": "version"}"#;
        let cmd: RpcCommand = from_str(json).unwrap();
        assert_eq!(cmd, RpcCommand::Version);
    }
}
