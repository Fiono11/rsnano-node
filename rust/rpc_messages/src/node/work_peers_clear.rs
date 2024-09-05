use crate::RpcCommand;

impl RpcCommand {
    pub fn work_peers_clear() -> Self {
        Self::WorkPeersClear
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use super::*;

    #[test]
    fn serialize_work_peers_clear() {
        let command = RpcCommand::work_peers_clear();
        let serialized = serde_json::to_value(command).unwrap();
        assert_eq!(serialized, json!({"action": "work_peers_clear"}));
    }

    #[test]
    fn deserialize_work_peers_clear() {
        let json = json!({"action": "work_peers_clear"});
        let deserialized: RpcCommand = serde_json::from_value(json).unwrap();
        assert!(matches!(deserialized, RpcCommand::WorkPeersClear));
    }
}