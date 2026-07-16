use std::{fmt::Display, io::Read, mem::size_of};

use bitvec::prelude::BitArray;
use num_traits::FromPrimitive;
use serde_derive::Serialize;

#[cfg(feature = "rai_protocol")]
use rsnano_types::RaiEpochCloseReq;
use rsnano_types::{Account, BlockHash, DeserializationError, HashOrAccount, read_u8, read_u64_be};
use rsnano_utils::stats::DetailType;

use super::MessageVariant;

/**
 * Type of requested asc pull data
 * - blocks:
 * - account_info:
 */
#[repr(u8)]
#[derive(Clone, FromPrimitive, Debug)]
pub enum AscPullPayloadId {
    Blocks = 0x1,
    AccountInfo = 0x2,
    Frontiers = 0x3,
    #[cfg(feature = "rai_protocol")]
    RaiEpochClose = 0x4,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "pull_type")]
pub enum AscPullReqType {
    Blocks(BlocksReqPayload),
    AccountInfo(AccountInfoReqPayload),
    Frontiers(FrontiersReqPayload),
    #[cfg(feature = "rai_protocol")]
    RaiEpochClose(RaiEpochCloseReq),
}

impl AscPullReqType {
    /// Query account info by block hash
    pub fn account_info_by_hash(next: BlockHash) -> AscPullReqType {
        AscPullReqType::AccountInfo(AccountInfoReqPayload {
            target: next.into(),
            target_type: HashType::Block,
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        match self {
            AscPullReqType::Blocks(i) => i.serialize(writer),
            AscPullReqType::AccountInfo(i) => i.serialize(writer),
            AscPullReqType::Frontiers(i) => i.serialize(writer),
            #[cfg(feature = "rai_protocol")]
            AscPullReqType::RaiEpochClose(i) => i.serialize(writer),
        }
    }

    pub const fn payload_type(&self) -> AscPullPayloadId {
        match self {
            AscPullReqType::Blocks(_) => AscPullPayloadId::Blocks,
            AscPullReqType::AccountInfo(_) => AscPullPayloadId::AccountInfo,
            AscPullReqType::Frontiers(_) => AscPullPayloadId::Frontiers,
            #[cfg(feature = "rai_protocol")]
            AscPullReqType::RaiEpochClose(_) => AscPullPayloadId::RaiEpochClose,
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
    fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        FromPrimitive::from_u8(read_u8(reader)?).ok_or(DeserializationError::InvalidData)
    }
}

#[derive(Default, Clone, PartialEq, Eq, Debug, Serialize)]
pub struct BlocksReqPayload {
    pub start_type: HashType,
    pub start: HashOrAccount,
    pub count: u8,
}

impl BlocksReqPayload {
    pub fn new_test_instance() -> Self {
        Self {
            start: HashOrAccount::from(123),
            count: 100,
            start_type: HashType::Account,
        }
    }

    fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let start = HashOrAccount::deserialize(reader)?;
        let count = read_u8(reader)?;
        let start_type = HashType::deserialize(reader)?;
        Ok(Self {
            start_type,
            start,
            count,
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        writer.write_all(self.start.as_bytes())?;
        writer.write_all(&[self.count, self.start_type as u8])
    }
}

#[derive(Default, Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AccountInfoReqPayload {
    pub target: HashOrAccount,
    pub target_type: HashType,
}

impl AccountInfoReqPayload {
    pub fn new_test_instance() -> Self {
        Self {
            target: HashOrAccount::from(42),
            target_type: HashType::Account,
        }
    }

    fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let target = HashOrAccount::deserialize(reader)?;
        let target_type = HashType::deserialize(reader)?;
        Ok(Self {
            target,
            target_type,
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        writer.write_all(self.target.as_bytes())?;
        writer.write_all(&[self.target_type as u8])
    }
}

#[derive(Default, Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct FrontiersReqPayload {
    pub start: Account,
    pub count: u16,
}

impl FrontiersReqPayload {
    /// Header allows for 16 bit extensions; 65536 bytes / 64 bytes (account + frontier) ~ 1024,
    /// but we need some space for null frontier terminator
    pub const MAX_FRONTIERS: u16 = 1000;

    fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let start = Account::deserialize(reader)?;
        let mut count_bytes = [0u8; 2];
        reader.read_exact(&mut count_bytes)?;
        let count = u16::from_be_bytes(count_bytes);
        Ok(Self { start, count })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.start.serialize(writer)?;
        let count_bytes = self.count.to_be_bytes();
        writer.write_all(&count_bytes)
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
            AscPullReqType::Frontiers(frontiers) => {
                write!(f, "\nstart:{} count:{}", frontiers.start, frontiers.count)?;
            }
            #[cfg(feature = "rai_protocol")]
            AscPullReqType::RaiEpochClose(request) => {
                write!(
                    f,
                    "\nepoch:{} start_index:{} max entries:{}",
                    request.epoch, request.start_index, request.max_entries
                )?;
            }
        }
        Ok(())
    }
}

impl AscPullReq {
    pub fn new_test_instance_blocks() -> Self {
        Self {
            id: 12345,
            req_type: AscPullReqType::Blocks(BlocksReqPayload::new_test_instance()),
        }
    }

    pub fn new_test_instance_account() -> Self {
        Self {
            id: 12345,
            req_type: AscPullReqType::AccountInfo(AccountInfoReqPayload::new_test_instance()),
        }
    }

    pub fn payload_type(&self) -> AscPullPayloadId {
        self.req_type.payload_type()
    }

    pub fn serialized_size(extensions: BitArray<u16>) -> usize {
        let payload_len = extensions.data as usize;
        size_of::<u8>() // pull type
        + size_of::<u64>() // id
        + payload_len
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        writer.write_all(&[self.payload_type() as u8])?;
        writer.write_all(&self.id.to_be_bytes())?;
        self.req_type.serialize(writer)
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let pull_type = AscPullPayloadId::from_u8(read_u8(&mut bytes)?)
            .ok_or(DeserializationError::InvalidData)?;
        let id = read_u64_be(&mut bytes)?;

        let req_type = match pull_type {
            AscPullPayloadId::Blocks => {
                let payload = BlocksReqPayload::deserialize(&mut bytes)?;
                AscPullReqType::Blocks(payload)
            }
            AscPullPayloadId::AccountInfo => {
                let payload = AccountInfoReqPayload::deserialize(&mut bytes)?;
                AscPullReqType::AccountInfo(payload)
            }
            AscPullPayloadId::Frontiers => {
                let payload = FrontiersReqPayload::deserialize(&mut bytes)?;
                AscPullReqType::Frontiers(payload)
            }
            #[cfg(feature = "rai_protocol")]
            AscPullPayloadId::RaiEpochClose => {
                let payload = RaiEpochCloseReq::deserialize(&mut bytes)?;
                AscPullReqType::RaiEpochClose(payload)
            }
        };
        Ok(Self { id, req_type })
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

impl From<&AscPullReqType> for DetailType {
    fn from(value: &AscPullReqType) -> Self {
        match value {
            AscPullReqType::Blocks(_) => DetailType::Blocks,
            AscPullReqType::AccountInfo(_) => DetailType::AccountInfo,
            AscPullReqType::Frontiers(_) => DetailType::Frontiers,
            #[cfg(feature = "rai_protocol")]
            AscPullReqType::RaiEpochClose(_) => DetailType::RaiEpochClose,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

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
    fn serialize_frontiers() {
        let original = Message::AscPullReq(AscPullReq {
            id: 7,
            req_type: AscPullReqType::Frontiers(FrontiersReqPayload {
                start: Account::from(42),
                count: 69,
            }),
        });
        assert_deserializable(&original);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn serialize_rai_epoch_close() {
        let original = Message::AscPullReq(AscPullReq {
            id: 7,
            req_type: AscPullReqType::RaiEpochClose(RaiEpochCloseReq::new(2, 512)),
        });
        assert_deserializable(&original);
    }
}
