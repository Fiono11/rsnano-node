use rsnano_core::{BlockHash, Networks};
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct VersionDto {
    pub rpc_version: u8,
    pub store_version: i32,
    pub protocol_version: u8,
    pub node_vendor: String,
    pub store_vendor: String,
    pub network: Networks,
    pub network_identifier: BlockHash,
    pub build_info: String,
}

impl VersionDto {
    pub fn new(
        rpc_version: u8,
        store_version: i32,
        protocol_version: u8,
        node_vendor: String,
        store_vendor: String,
        network: Networks,
        network_identifier: BlockHash,
        build_info: String,
    ) -> Self {
        Self {
            rpc_version,
            store_version,
            protocol_version,
            node_vendor,
            store_vendor,
            network,
            network_identifier,
            build_info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_str, to_string};

    #[test]
    fn serialize_version_dto() {
        let rpc_version = 1;
        let store_version = 1;
        let protocol_version = 1;
        let node_vendor = "node".to_string();
        let store_vendor = "store".to_string();
        let network = Networks::NanoLiveNetwork;
        let network_identifier = BlockHash::zero();
        let build_info = "build_info".to_string();

        let dto = VersionDto::new(
            rpc_version,
            store_version,
            protocol_version,
            node_vendor,
            store_vendor,
            network,
            network_identifier,
            build_info,
        );

        let serialized = to_string(&dto).unwrap();

        let expected_json = serde_json::json!({
            "rpc_version": 1,
            "store_version": 1,
            "protocol_version": 1,
            "node_vendor": "node",
            "store_vendor": "store",
            "network": "live",
            "network_identifier": "0000000000000000000000000000000000000000000000000000000000000000",
            "build_info": "build_info"
        });

        let actual_json: serde_json::Value = from_str(&serialized).unwrap();
        assert_eq!(actual_json, expected_json);
    }

    #[test]
    fn deserialize_version_dto() {
        let json_str = r#"{
            "rpc_version": 1,
            "store_version": 1,
            "protocol_version": 1,
            "node_vendor": "node",
            "store_vendor": "store",
            "network": "live",
            "network_identifier": "0000000000000000000000000000000000000000000000000000000000000000",
            "build_info": "build_info"
        }"#;

        let deserialized: VersionDto = from_str(json_str).unwrap();

        let rpc_version = 1;
        let store_version = 1;
        let protocol_version = 1;
        let node_vendor = "node".to_string();
        let store_vendor = "store".to_string();
        let network = Networks::NanoLiveNetwork;
        let network_identifier = BlockHash::zero();
        let build_info = "build_info".to_string();

        let expected = VersionDto::new(
            rpc_version,
            store_version,
            protocol_version,
            node_vendor,
            store_vendor,
            network,
            network_identifier,
            build_info,
        );

        assert_eq!(deserialized, expected);
    }
}
