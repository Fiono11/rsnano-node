use super::MessageVariant;
use bitvec::prelude::BitArray;
use num_traits::FromPrimitive;
use rsnano_types::{Block, BlockType, BlockTypeId, DeserializationError, serialized_block_size};
use serde_derive::Serialize;
use std::fmt::{Debug, Display};

#[derive(Clone, Eq, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct Publish {
    pub block: Block,

    /// Messages deserialized from network should have their digest set
    #[serde(skip_serializing)]
    pub digest: u128,

    pub is_originator: bool,
    pub is_recovery: bool,
}

impl Publish {
    const BLOCK_TYPE_MASK: u16 = 0x0f00;
    const ORIGINATOR_FLAG: u16 = 1 << 2;
    const RECOVERY_FLAG: u16 = 1 << 3;

    pub fn new_from_originator(block: Block) -> Self {
        Self {
            block,
            digest: 0,
            is_originator: true,
            is_recovery: false,
        }
    }

    pub fn new_forward(block: Block) -> Self {
        Self {
            block,
            digest: 0,
            is_originator: false,
            is_recovery: false,
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_recovery(block: Block) -> Self {
        Self {
            block,
            digest: 0,
            is_originator: true,
            is_recovery: true,
        }
    }

    pub fn is_recovery_extension(extensions: BitArray<u16>) -> bool {
        extensions.data & Self::RECOVERY_FLAG > 0
    }

    pub fn new_test_instance() -> Self {
        Self {
            block: Block::new_test_instance(),
            digest: 0,
            is_originator: true,
            is_recovery: false,
        }
    }

    pub fn serialized_size(extensions: BitArray<u16>) -> usize {
        let type_id = Self::block_type_id(extensions);
        match BlockType::try_from(type_id) {
            Ok(block_type) => serialized_block_size(block_type),
            Err(_) => 0,
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.block.serialize_without_block_type(writer)
    }

    pub fn deserialize(
        mut bytes: &[u8],
        extensions: BitArray<u16>,
        digest: u128,
    ) -> Result<Self, DeserializationError> {
        let type_id = Self::block_type_id(extensions);
        let block_type =
            BlockType::try_from(type_id).map_err(|_| DeserializationError::InvalidData)?;

        let payload = Publish {
            block: Block::deserialize_block_type(block_type, &mut bytes)?,
            digest,
            is_originator: extensions.data & Self::ORIGINATOR_FLAG > 0,
            is_recovery: Self::is_recovery_extension(extensions),
        };

        Ok(payload)
    }

    fn block_type_id(extensions: BitArray<u16>) -> BlockTypeId {
        let mut value = extensions & BitArray::new(Self::BLOCK_TYPE_MASK);
        value.shift_left(8);
        FromPrimitive::from_u16(value.data).unwrap_or(BlockTypeId::Invalid)
    }
}

impl PartialEq for Publish {
    fn eq(&self, other: &Self) -> bool {
        self.block == other.block
    }
}

impl MessageVariant for Publish {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        let type_id = BlockTypeId::from(self.block.block_type());
        let mut flags = (type_id as u16) << 8;
        if self.is_originator {
            flags |= Self::ORIGINATOR_FLAG;
        }
        if self.is_recovery {
            flags |= Self::RECOVERY_FLAG;
        }
        BitArray::new(flags)
    }
}

impl Display for Publish {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\n{}",
            self.block.to_json().map_err(|_| std::fmt::Error)?
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::TestBlockBuilder;

    #[test]
    fn create_from_originator() {
        let publish = Publish::new_from_originator(Block::new_test_instance());
        assert_eq!(publish.is_originator, true)
    }

    #[test]
    fn create_forward() {
        let publish = Publish::new_forward(Block::new_test_instance());
        assert_eq!(publish.is_originator, false);
    }

    #[test]
    fn originator_flag_in_header() {
        let publish = Publish::new_from_originator(Block::new_test_instance());
        let flags = publish.header_extensions(0);
        assert!(flags.data & Publish::ORIGINATOR_FLAG > 0);
    }

    #[test]
    fn originator_flag_not_in_header() {
        let publish = Publish::new_forward(Block::new_test_instance());
        let flags = publish.header_extensions(0);
        assert_eq!(flags.data & Publish::ORIGINATOR_FLAG, 0);
    }

    #[test]
    fn serialize() {
        let block = TestBlockBuilder::state().build();
        let mut publish1 = Publish::new_from_originator(block);
        publish1.digest = 123;

        let mut buffer = Vec::new();
        publish1.serialize(&mut buffer).unwrap();

        let extensions = publish1.header_extensions(0);
        let publish2 = Publish::deserialize(&mut buffer.as_slice(), extensions, 123).unwrap();
        assert_eq!(publish1, publish2);
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn recovery_flag_roundtrip() {
        let publish1 = Publish::new_recovery(Block::new_test_instance());
        let mut buffer = Vec::new();
        publish1.serialize(&mut buffer).unwrap();
        let extensions = publish1.header_extensions(0);
        let publish2 = Publish::deserialize(&buffer, extensions, 0).unwrap();
        assert!(publish2.is_recovery);
        assert_eq!(publish1, publish2);
    }
}
