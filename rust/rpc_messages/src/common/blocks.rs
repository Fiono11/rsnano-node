use rsnano_core::BlockHash;
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BlockHashesDto {
    pub blocks: Vec<BlockHash>,
}

impl BlockHashesDto {
    pub fn new(blocks: Vec<BlockHash>) -> Self {
        Self { blocks }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_str, to_string};

    #[test]
    fn serialize_blocks_dto() {
        let dto = BlockHashesDto::new(vec![BlockHash::zero()]);

        let serialized = to_string(&dto).unwrap();

        let expected_json = serde_json::json!({
            "blocks": ["0000000000000000000000000000000000000000000000000000000000000000"]
        });

        let actual_json: serde_json::Value = from_str(&serialized).unwrap();
        assert_eq!(actual_json, expected_json);
    }

    #[test]
    fn deserialize_blocks_dto() {
        let json_str = r#"{
            "blocks": ["0000000000000000000000000000000000000000000000000000000000000000"]
        }"#;

        let deserialized: BlockHashesDto = from_str(json_str).unwrap();

        let expected = BlockHashesDto::new(vec![BlockHash::zero()]);

        assert_eq!(deserialized, expected);
    }
}
