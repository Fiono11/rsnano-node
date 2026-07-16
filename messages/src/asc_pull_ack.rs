use std::{collections::VecDeque, fmt::Display, io::Read, mem::size_of};

use bitvec::prelude::BitArray;
use num_traits::FromPrimitive;
use serde::ser::SerializeStruct;
use serde_derive::Serialize;

#[cfg(feature = "rai_protocol")]
use rsnano_types::RaiEpochCloseAck;
use rsnano_types::{
    Account, Block, BlockHash, BlockType, BlockTypeId, DeserializationError, Frontier, read_u8,
    read_u64_be,
};
use rsnano_utils::stats::DetailType;

use super::{AscPullPayloadId, MessageVariant};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AscPullAckType {
    Blocks(BlocksAckPayload),
    AccountInfo(AccountInfoAckPayload),
    Frontiers(Vec<Frontier>),
    #[cfg(feature = "rai_protocol")]
    RaiEpochClose(RaiEpochCloseAck),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AscPullAck {
    pub id: u64,
    pub pull_type: AscPullAckType,
}

impl AscPullAck {
    pub const MAX_FRONTIERS: usize = 1000;

    pub fn new_test_instance_blocks() -> Self {
        Self {
            id: 12345,
            pull_type: AscPullAckType::Blocks(BlocksAckPayload(VecDeque::from([
                Block::new_test_instance(),
            ]))),
        }
    }

    pub fn new_test_instance_account() -> Self {
        Self {
            id: 12345,
            pull_type: AscPullAckType::AccountInfo(AccountInfoAckPayload::new_test_instance()),
        }
    }

    pub fn payload_type(&self) -> AscPullPayloadId {
        match self.pull_type {
            AscPullAckType::Blocks(_) => AscPullPayloadId::Blocks,
            AscPullAckType::AccountInfo(_) => AscPullPayloadId::AccountInfo,
            AscPullAckType::Frontiers(_) => AscPullPayloadId::Frontiers,
            #[cfg(feature = "rai_protocol")]
            AscPullAckType::RaiEpochClose(_) => AscPullPayloadId::RaiEpochClose,
        }
    }

    pub fn serialized_size(extensions: BitArray<u16>) -> usize {
        let payload_length = extensions.data as usize;

        size_of::<u8>() // type code 
        + size_of::<u64>() // id
        + payload_length
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        writer.write_all(&[self.payload_type() as u8])?;
        writer.write_all(&self.id.to_be_bytes())?;
        self.serialize_pull_type(writer)
    }

    fn serialize_pull_type<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        match &self.pull_type {
            AscPullAckType::Blocks(blocks) => blocks.serialize(writer),
            AscPullAckType::AccountInfo(account_info) => account_info.serialize(writer),
            AscPullAckType::Frontiers(frontiers) => {
                debug_assert!(frontiers.len() <= Self::MAX_FRONTIERS);
                for frontier in frontiers {
                    frontier.serialize(writer)?;
                }
                Frontier::default().serialize(writer)
            }
            #[cfg(feature = "rai_protocol")]
            AscPullAckType::RaiEpochClose(ack) => ack.serialize(writer),
        }
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let pull_type_code = AscPullPayloadId::from_u8(read_u8(&mut bytes)?)
            .ok_or(DeserializationError::InvalidData)?;
        let id = read_u64_be(&mut bytes)?;
        let pull_type = match pull_type_code {
            AscPullPayloadId::Blocks => {
                let payload = BlocksAckPayload::deserialize(bytes)?;
                AscPullAckType::Blocks(payload)
            }
            AscPullPayloadId::AccountInfo => {
                let payload = AccountInfoAckPayload::deserialize(&mut bytes)?;
                AscPullAckType::AccountInfo(payload)
            }
            AscPullPayloadId::Frontiers => {
                let mut frontiers = Vec::new();
                while frontiers.len() < Self::MAX_FRONTIERS {
                    let current = Frontier::deserialize(&mut bytes)?;
                    if current == Default::default() {
                        break;
                    }
                    frontiers.push(current);
                }

                AscPullAckType::Frontiers(frontiers)
            }
            #[cfg(feature = "rai_protocol")]
            AscPullPayloadId::RaiEpochClose => {
                AscPullAckType::RaiEpochClose(RaiEpochCloseAck::deserialize(bytes)?)
            }
        };

        Ok(AscPullAck { id, pull_type })
    }
}

impl MessageVariant for AscPullAck {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(
            payload_len
            -1 // pull_type
            - 8, // ID
        )
    }
}

impl Display for AscPullAck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.pull_type {
            AscPullAckType::Blocks(blocks) => {
                for block in blocks.blocks() {
                    write!(f, "{}", block.to_json().map_err(|_| std::fmt::Error)?)?;
                }
            }
            AscPullAckType::AccountInfo(info) => {
                write!(
                    f,
                    "\naccount public key:{} account open:{} account head:{} block count:{} confirmation frontier:{} confirmation height:{}",
                    info.account.encode_account(),
                    info.account_open,
                    info.account_head,
                    info.account_block_count,
                    info.account_conf_frontier,
                    info.account_conf_height,
                )?;
            }
            AscPullAckType::Frontiers(_) => {}
            #[cfg(feature = "rai_protocol")]
            AscPullAckType::RaiEpochClose(ack) => {
                if let Some(page) = &ack.page {
                    write!(
                        f,
                        "\nepoch close state page:{} start:{} count:{} total:{}",
                        page.epoch,
                        page.start_index,
                        page.entries.len(),
                        page.total_entries
                    )?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct BlocksAckPayload(VecDeque<Block>);

impl BlocksAckPayload {
    pub fn new(blocks: VecDeque<Block>) -> Self {
        if blocks.len() > Self::MAX_BLOCKS as usize {
            panic!(
                "too many blocks for BlocksAckPayload. Maximum is {}, but was {}",
                Self::MAX_BLOCKS,
                blocks.len()
            );
        }
        Self(blocks)
    }

    pub fn new_test_instance() -> Self {
        let mut blocks = VecDeque::new();
        blocks.push_back(Block::new_test_instance());
        Self::new(blocks)
    }

    pub fn empty() -> Self {
        Self::new(VecDeque::new())
    }

    /* Header allows for 16 bit extensions; 65535 bytes / 500 bytes (block size with some future margin) ~ 131 */
    pub const MAX_BLOCKS: u8 = 128;

    pub fn blocks(&self) -> &VecDeque<Block> {
        &self.0
    }

    pub fn take_blocks(self) -> VecDeque<Block> {
        self.0
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        for block in self.blocks() {
            block.serialize(writer)?;
        }
        // For convenience, end with null block terminator
        writer.write_all(&[BlockTypeId::NotABlock as u8])
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let mut blocks = VecDeque::new();

        while !bytes.is_empty() {
            let type_id = BlockTypeId::from_u8(read_u8(&mut bytes)?)
                .ok_or(DeserializationError::InvalidData)?;

            if type_id == BlockTypeId::NotABlock {
                break;
            }

            let block_type =
                BlockType::try_from(type_id).map_err(|_| DeserializationError::InvalidData)?;

            let block = Block::deserialize_block_type(block_type, &mut bytes)?;
            blocks.push_back(block);

            if blocks.len() > Self::MAX_BLOCKS as usize {
                return Err(DeserializationError::TooMuchData);
            }
        }

        Ok(Self::new(blocks))
    }
}

impl serde::Serialize for BlocksAckPayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Block", 6)?;
        state.serialize_field("blocks", &self.0)?;
        state.end()
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize)]
pub struct AccountInfoAckPayload {
    pub account: Account,
    pub account_open: BlockHash,
    pub account_head: BlockHash,
    pub account_block_count: u64,
    pub account_conf_frontier: BlockHash,
    pub account_conf_height: u64,
}

impl AccountInfoAckPayload {
    pub fn new_test_instance() -> AccountInfoAckPayload {
        Self {
            account: Account::from(1),
            account_open: BlockHash::from(2),
            account_head: BlockHash::from(3),
            account_block_count: 4,
            account_conf_frontier: BlockHash::from(5),
            account_conf_height: 3,
        }
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let account = Account::deserialize(reader)?;
        let account_open = BlockHash::deserialize(reader)?;
        let account_head = BlockHash::deserialize(reader)?;
        let account_block_count = read_u64_be(reader)?;
        let account_conf_frontier = BlockHash::deserialize(reader)?;
        let account_conf_height = read_u64_be(reader)?;

        Ok(Self {
            account,
            account_open,
            account_head,
            account_block_count,
            account_conf_frontier,
            account_conf_height,
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.account.serialize(writer)?;
        self.account_open.serialize(writer)?;
        self.account_head.serialize(writer)?;
        writer.write_all(&self.account_block_count.to_be_bytes())?;
        self.account_conf_frontier.serialize(writer)?;
        writer.write_all(&self.account_conf_height.to_be_bytes())
    }
}

impl From<&AscPullAckType> for DetailType {
    fn from(value: &AscPullAckType) -> Self {
        match value {
            AscPullAckType::Blocks(_) => DetailType::Blocks,
            AscPullAckType::AccountInfo(_) => DetailType::AccountInfo,
            AscPullAckType::Frontiers(_) => DetailType::Frontiers,
            #[cfg(feature = "rai_protocol")]
            AscPullAckType::RaiEpochClose(_) => DetailType::RaiEpochClose,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};
    use rsnano_types::TestBlockBuilder;

    #[test]
    fn serialize_blocks() {
        let original = Message::AscPullAck(AscPullAck {
            id: 7,
            pull_type: AscPullAckType::Blocks(BlocksAckPayload::new(VecDeque::from([
                TestBlockBuilder::state().build(),
                TestBlockBuilder::state().build(),
            ]))),
        });

        assert_deserializable(&original);
    }

    #[test]
    fn serialize_account_info() {
        let original = Message::AscPullAck(AscPullAck {
            id: 7,
            pull_type: AscPullAckType::AccountInfo(AccountInfoAckPayload {
                account: Account::from(1),
                account_open: BlockHash::from(2),
                account_head: BlockHash::from(3),
                account_block_count: 4,
                account_conf_frontier: BlockHash::from(5),
                account_conf_height: 6,
            }),
        });

        assert_deserializable(&original);
    }

    #[test]
    fn serialize_frontiers() {
        let original = Message::AscPullAck(AscPullAck {
            id: 7,
            pull_type: AscPullAckType::Frontiers(vec![Frontier::new(
                Account::from(1),
                BlockHash::from(2),
            )]),
        });

        assert_deserializable(&original);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn serialize_rai_epoch_close() {
        use rsnano_types::{
            RaiEpochCloseEntry, RaiEpochCloseEntryState, RaiEpochClosePage, RaiSlot,
        };

        let original = Message::AscPullAck(AscPullAck {
            id: 7,
            pull_type: AscPullAckType::RaiEpochClose(RaiEpochCloseAck::new(
                RaiEpochClosePage::new(
                    2,
                    1,
                    0,
                    BlockHash::from(9),
                    vec![RaiEpochCloseEntry {
                        slot: RaiSlot::new(Account::from(1), 2),
                        state: RaiEpochCloseEntryState::Finalized(BlockHash::from(3)),
                    }],
                ),
            )),
        });

        assert_deserializable(&original);
    }
}
