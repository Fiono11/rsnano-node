use super::{Block, BlockBase, BlockType, BlockTypeId};
use crate::{
    Account, Amount, Blake2HashBuilder, BlockHash, DependentBlocks, DeserializationError,
    JsonBlock, Link, PublicKey, Root, Signature, WorkNonce,
};
use std::io::Read;
use std::sync::LazyLock;

#[derive(Clone, Default, Debug)]
pub struct DummyBlock {
    hashables: DummyHashables,
    hash: BlockHash,
}

impl DummyBlock {
    pub const SERIALIZED_SIZE: usize = DummyHashables::SERIALIZED_SIZE;

    pub fn new_test_instance() -> Self {
        DummyBlockArgs {
            previous: BlockHash::ZERO,
        }
        .into()
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let hashables = DummyHashables::deserialize(reader)?;
        let hash = hashables.hash();
        Ok(DummyBlock { hashables, hash })
    }

    pub fn dependent_blocks(&self) -> DependentBlocks {
        DependentBlocks::new(self.previous(), BlockHash::ZERO)
    }

    pub fn serialize_without_block_type<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.hashables.serialize(writer)
    }
}

impl PartialEq for DummyBlock {
    fn eq(&self, other: &Self) -> bool {
        self.hashables == other.hashables
    }
}

impl Eq for DummyBlock {}

impl BlockBase for DummyBlock {
    fn block_type(&self) -> BlockType {
        BlockType::Dummy
    }

    fn account_field(&self) -> Option<Account> {
        None
    }

    fn hash(&self) -> BlockHash {
        self.hash
    }

    fn link_field(&self) -> Option<Link> {
        None
    }

    fn signature(&self) -> &Signature {
        // DummyBlock doesn't have a signature, return a static zero signature
        static ZERO_SIG: LazyLock<Signature> = LazyLock::new(|| Signature::default());
        &ZERO_SIG
    }

    fn set_signature(&mut self, _signature: Signature) {
        // DummyBlock doesn't store signatures, this is a no-op
    }

    fn set_work(&mut self, _work: WorkNonce) {
        // DummyBlock doesn't store work, this is a no-op
    }

    fn work(&self) -> WorkNonce {
        WorkNonce::default()
    }

    fn previous(&self) -> BlockHash {
        self.hashables.previous
    }

    fn root(&self) -> Root {
        self.previous().into()
    }

    fn balance_field(&self) -> Option<Amount> {
        None
    }

    fn source_field(&self) -> Option<BlockHash> {
        None
    }

    fn representative_field(&self) -> Option<PublicKey> {
        None
    }

    fn valid_predecessor(&self, _block_type: BlockType) -> bool {
        true
    }

    fn destination_field(&self) -> Option<Account> {
        None
    }

    fn json_representation(&self) -> JsonBlock {
        JsonBlock::Dummy(JsonDummyBlock {
            previous: self.hashables.previous,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
struct DummyHashables {
    pub previous: BlockHash,
}

impl DummyHashables {
    pub const SERIALIZED_SIZE: usize = BlockHash::SERIALIZED_SIZE;

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.previous.serialize(writer)
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let previous = BlockHash::deserialize(reader)?;
        Ok(Self { previous })
    }

    fn hash(&self) -> BlockHash {
        let mut preamble = [0u8; 32];
        preamble[31] = BlockTypeId::Dummy as u8;
        Blake2HashBuilder::new()
            .update(preamble)
            .update(self.previous.as_bytes())
            .build()
    }
}

pub struct DummyBlockArgs {
    pub previous: BlockHash,
}

impl From<DummyBlockArgs> for DummyBlock {
    fn from(value: DummyBlockArgs) -> Self {
        let hashables = DummyHashables {
            previous: value.previous,
        };

        let hash = hashables.hash();

        Self { hashables, hash }
    }
}

impl From<DummyBlockArgs> for Block {
    fn from(value: DummyBlockArgs) -> Self {
        Block::Dummy(value.into())
    }
}

#[derive(PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct JsonDummyBlock {
    pub previous: BlockHash,
}

impl From<JsonDummyBlock> for DummyBlock {
    fn from(value: JsonDummyBlock) -> Self {
        let hashables = DummyHashables {
            previous: value.previous,
        };

        let hash = hashables.hash();

        Self { hashables, hash }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize() {
        let block1: DummyBlock = DummyBlockArgs { previous: 0.into() }.into();
        let mut buffer = Vec::new();
        block1.serialize_without_block_type(&mut buffer).unwrap();
        assert_eq!(DummyBlock::SERIALIZED_SIZE, buffer.len());

        let block2 = DummyBlock::deserialize(&mut buffer.as_slice()).unwrap();
        assert_eq!(block1, block2);
    }
}
