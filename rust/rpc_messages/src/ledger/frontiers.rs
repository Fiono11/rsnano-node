use crate::{AccountWithCountArgs, RpcCommand};
use rsnano_core::Account;

impl RpcCommand {
    pub fn frontiers(account: Account, count: u64) -> Self {
        Self::Frontiers(AccountWithCountArgs::new(account, count))
    }
}

#[cfg(test)]
mod tests {
    use crate::RpcCommand;
    use rsnano_core::Account;
    use serde_json::to_string_pretty;

    #[test]
    fn serialize_frontiers_command() {
        assert_eq!(
            to_string_pretty(&RpcCommand::frontiers(Account::zero(), 1)).unwrap(),
            r#"{
  "action": "frontiers",
  "account": "nano_1111111111111111111111111111111111111111111111111111hifc8npp",
  "count": 1
}"#
        )
    }

    #[test]
    fn deserialize_frontiers_command() {
        let json_str = r#"{
  "action": "frontiers",
  "account": "nano_1111111111111111111111111111111111111111111111111111hifc8npp",
  "count": 1
    }"#;
        let deserialized: RpcCommand = serde_json::from_str(json_str).unwrap();
        let expected_command = RpcCommand::frontiers(Account::zero(), 1);
        assert_eq!(deserialized, expected_command);
    }
}
