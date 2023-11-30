use super::MessageVariant;
use bitvec::prelude::BitArray;
use num_traits::FromPrimitive;
use rsnano_core::{
    utils::{BufferWriter, Deserialize, Serialize, Stream, StreamExt},
    HashOrAccount,
};
use serde_derive::Serialize;
use std::{fmt::Display, mem::size_of};

/**
 * Type of requested asc pull data
 * - blocks:
 * - account_info:
 */
#[repr(u8)]
#[derive(Clone, FromPrimitive)]
pub enum AscPullPayloadId {
    Blocks = 0x1,
    AccountInfo = 0x2,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "pull_type")]
pub enum AscPullReqType {
    Blocks(BlocksReqPayload),
    AccountInfo(AccountInfoReqPayload),
}

impl Serialize for AscPullReqType {
    fn serialize(&self, writer: &mut dyn BufferWriter) {
        match &self {
            AscPullReqType::Blocks(blocks) => blocks.serialize(writer),
            AscPullReqType::AccountInfo(account_info) => account_info.serialize(writer),
        }
    }
}

#[derive(FromPrimitive, PartialEq, Eq, Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HashType {
    #[default]
    Account = 0,
    Block = 1,
}

impl HashType {
    fn deserialize(stream: &mut dyn Stream) -> anyhow::Result<Self> {
        FromPrimitive::from_u8(stream.read_u8()?).ok_or_else(|| anyhow!("target_type missing"))
    }
}

#[derive(Default, Clone, PartialEq, Eq, Debug, Serialize)]
pub struct BlocksReqPayload {
    pub start_type: HashType,
    pub start: HashOrAccount,
    pub count: u8,
}

impl BlocksReqPayload {
    pub fn create_test_instance() -> Self {
        Self {
            start: HashOrAccount::from(123),
            count: 100,
            start_type: HashType::Account,
        }
    }

    fn deserialize(&mut self, stream: &mut dyn Stream) -> anyhow::Result<()> {
        self.start = HashOrAccount::deserialize(stream)?;
        self.count = stream.read_u8()?;
        self.start_type = HashType::deserialize(stream)?;
        Ok(())
    }
}

impl Serialize for BlocksReqPayload {
    fn serialize(&self, writer: &mut dyn BufferWriter) {
        writer.write_bytes_safe(self.start.as_bytes());
        writer.write_u8_safe(self.count);
        writer.write_u8_safe(self.start_type as u8);
    }
}

#[derive(Default, Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AccountInfoReqPayload {
    pub target: HashOrAccount,
    pub target_type: HashType,
}

impl AccountInfoReqPayload {
    fn deserialize(&mut self, stream: &mut dyn Stream) -> anyhow::Result<()> {
        self.target = HashOrAccount::deserialize(stream)?;
        self.target_type = HashType::deserialize(stream)?;
        Ok(())
    }

    pub fn create_test_instance() -> Self {
        Self {
            target: HashOrAccount::from(42),
            target_type: HashType::Account,
        }
    }
}

impl Serialize for AccountInfoReqPayload {
    fn serialize(&self, writer: &mut dyn BufferWriter) {
        writer.write_bytes_safe(self.target.as_bytes());
        writer.write_u8_safe(self.target_type as u8);
    }
}

/// Ascending bootstrap pull request
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AscPullReq {
    pub id: u64,
    #[serde(flatten)]
    pub req_type: AscPullReqType,
}

impl Display for AscPullReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.req_type {
            AscPullReqType::Blocks(blocks) => {
                write!(
                    f,
                    "\nacc:{} max block count:{} hash type: {}",
                    blocks.start, blocks.count, blocks.start_type as u8
                )?;
            }
            AscPullReqType::AccountInfo(info) => {
                write!(
                    f,
                    "\ntarget:{} hash type:{}",
                    info.target, info.target_type as u8
                )?;
            }
        }
        Ok(())
    }
}

impl AscPullReq {
    pub fn create_test_instance_blocks() -> Self {
        Self {
            id: 12345,
            req_type: AscPullReqType::Blocks(BlocksReqPayload::create_test_instance()),
        }
    }

    pub fn create_test_instance_account() -> Self {
        Self {
            id: 12345,
            req_type: AscPullReqType::AccountInfo(AccountInfoReqPayload::create_test_instance()),
        }
    }

    pub fn deserialize(stream: &mut impl Stream) -> Option<Self> {
        let pull_type = AscPullPayloadId::from_u8(stream.read_u8().ok()?)?;
        let id = stream.read_u64_be().ok()?;

        let req_type = match pull_type {
            AscPullPayloadId::Blocks => {
                let mut payload = BlocksReqPayload::default();
                payload.deserialize(stream).ok()?;
                AscPullReqType::Blocks(payload)
            }
            AscPullPayloadId::AccountInfo => {
                let mut payload = AccountInfoReqPayload::default();
                payload.deserialize(stream).ok()?;
                AscPullReqType::AccountInfo(payload)
            }
        };
        Some(Self { id, req_type })
    }

    pub fn payload_type(&self) -> AscPullPayloadId {
        match &self.req_type {
            AscPullReqType::Blocks(_) => AscPullPayloadId::Blocks,
            AscPullReqType::AccountInfo(_) => AscPullPayloadId::AccountInfo,
        }
    }

    pub fn serialized_size(extensions: BitArray<u16>) -> usize {
        let payload_len = extensions.data as usize;
        size_of::<u8>() // pull type
        + size_of::<u64>() // id
        + payload_len
    }
}

impl Serialize for AscPullReq {
    fn serialize(&self, writer: &mut dyn BufferWriter) {
        writer.write_u8_safe(self.payload_type() as u8);
        writer.write_u64_be_safe(self.id);
        self.req_type.serialize(writer);
    }
}

impl MessageVariant for AscPullReq {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(
            payload_len
            -1 // pull_type
            - 8, // ID
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assert_deserializable, Message};

    #[test]
    fn serialize_blocks() {
        let original = Message::AscPullReq(AscPullReq {
            id: 7,
            req_type: AscPullReqType::Blocks(BlocksReqPayload {
                start: HashOrAccount::from(3),
                count: 111,
                start_type: HashType::Block,
            }),
        });

        assert_deserializable(&original);
    }

    #[test]
    fn serialize_account_info() {
        let original = Message::AscPullReq(AscPullReq {
            id: 7,
            req_type: AscPullReqType::AccountInfo(AccountInfoReqPayload {
                target: HashOrAccount::from(123),
                target_type: HashType::Block,
            }),
        });

        assert_deserializable(&original);
    }

    #[test]
    fn display_blocks_payload() {
        let req = Message::AscPullReq(AscPullReq {
            req_type: AscPullReqType::Blocks(BlocksReqPayload {
                start: 1.into(),
                count: 2,
                start_type: HashType::Block,
            }),
            id: 7,
        });
        assert_eq!(req.to_string(), "\nacc:0000000000000000000000000000000000000000000000000000000000000001 max block count:2 hash type: 1");
    }

    #[test]
    fn display_account_info_payload() {
        let req = Message::AscPullReq(AscPullReq {
            req_type: AscPullReqType::AccountInfo(AccountInfoReqPayload {
                target: HashOrAccount::from(123),
                target_type: HashType::Block,
            }),
            id: 7,
        });
        assert_eq!(
            req.to_string(),
            "\ntarget:000000000000000000000000000000000000000000000000000000000000007B hash type:1"
        );
    }

    #[test]
    fn serialize_json_blocks() {
        let serialized = serde_json::to_string_pretty(&Message::AscPullReq(
            AscPullReq::create_test_instance_blocks(),
        ))
        .unwrap();

        assert_eq!(
            serialized,
            r#"{
  "message_type": "asc_pull_req",
  "id": 12345,
  "pull_type": "blocks",
  "start_type": "account",
  "start": "000000000000000000000000000000000000000000000000000000000000007B",
  "count": 100
}"#
        );
    }

    #[test]
    fn serialize_json_account() {
        let serialized = serde_json::to_string_pretty(&Message::AscPullReq(
            AscPullReq::create_test_instance_account(),
        ))
        .unwrap();

        assert_eq!(
            serialized,
            r#"{
  "message_type": "asc_pull_req",
  "id": 12345,
  "pull_type": "account_info",
  "target": "000000000000000000000000000000000000000000000000000000000000002A",
  "target_type": "account"
}"#
        );
    }
}