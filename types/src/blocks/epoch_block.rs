use super::{Block, BlockBase, BlockType, BlockTypeId};
use crate::{
    Account, Amount, Blake2HashBuilder, BlockHash, DependentBlocks, DeserializationError,
    JsonBlock, Link, PublicKey, Root, Signature, WorkNonce,
};
use std::io::Read;
use std::sync::LazyLock;

#[derive(Clone, Default, Debug)]
pub struct EpochBlock {
    hashables: EpochHashables,
    hash: BlockHash,
}

impl EpochBlock {
    pub fn serialized_size(representatives_count: usize) -> usize {
        EpochHashables::serialized_size(representatives_count)
    }

    pub fn new_test_instance() -> Self {
        EpochBlockArgs {
            previous: BlockHash::ZERO,
            epoch: 0,
            representatives: vec![(789.into(), 420.into())],
        }
        .into()
    }

    pub fn epoch(&self) -> u128 {
        self.hashables.epoch
    }

    pub fn representatives(&self) -> &[(PublicKey, Amount)] {
        &self.hashables.representatives
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let hashables = EpochHashables::deserialize(reader)?;
        let hash = hashables.hash();
        Ok(Self { hashables, hash })
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

impl PartialEq for EpochBlock {
    fn eq(&self, other: &Self) -> bool {
        self.hashables == other.hashables
    }
}

impl Eq for EpochBlock {}

impl BlockBase for EpochBlock {
    fn block_type(&self) -> BlockType {
        BlockType::Epoch
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
        // EpochBlock doesn't have a signature, return a static zero signature
        static ZERO_SIG: LazyLock<Signature> = LazyLock::new(|| Signature::default());
        &ZERO_SIG
    }

    fn set_signature(&mut self, _signature: Signature) {
        // EpochBlock doesn't store signatures, this is a no-op
    }

    fn set_work(&mut self, _work: WorkNonce) {
        // EpochBlock doesn't store work, this is a no-op
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
        self.hashables.representatives.first().map(|(pk, _)| *pk)
    }

    fn valid_predecessor(&self, _block_type: BlockType) -> bool {
        true
    }

    fn destination_field(&self) -> Option<Account> {
        None
    }

    fn json_representation(&self) -> JsonBlock {
        JsonBlock::Epoch(JsonEpochBlock {
            previous: self.hashables.previous,
            epoch: self.hashables.epoch,
            representatives: self
                .hashables
                .representatives
                .iter()
                .map(|(pk, amount)| (pk.into(), *amount))
                .collect(),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
struct EpochHashables {
    previous: BlockHash,
    epoch: u128,
    representatives: Vec<(PublicKey, Amount)>,
}

impl EpochHashables {
    pub fn serialized_size(representatives_count: usize) -> usize {
        BlockHash::SERIALIZED_SIZE
            + 16 // u128 for epoch
            + 2 // u16 for count
            + representatives_count * (PublicKey::SERIALIZED_SIZE + Amount::SERIALIZED_SIZE)
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.previous.serialize(writer)?;
        writer.write_all(&self.epoch.to_be_bytes())?;
        writer.write_all(&(self.representatives.len() as u16).to_be_bytes())?;
        for (public_key, amount) in &self.representatives {
            public_key.serialize(writer)?;
            amount.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let previous = BlockHash::deserialize(reader)?;
        let mut epoch_buffer = [0u8; 16];
        reader.read_exact(&mut epoch_buffer)?;
        let epoch = u128::from_be_bytes(epoch_buffer);
        let mut count_buffer = [0u8; 2];
        reader.read_exact(&mut count_buffer)?;
        let count = u16::from_be_bytes(count_buffer) as usize;
        let mut representatives = Vec::with_capacity(count);
        for _ in 0..count {
            let public_key = PublicKey::deserialize(reader)?;
            let amount = Amount::deserialize(reader)?;
            representatives.push((public_key, amount));
        }
        Ok(Self {
            previous,
            epoch,
            representatives,
        })
    }

    fn hash(&self) -> BlockHash {
        let mut preamble = [0u8; 32];
        preamble[31] = BlockTypeId::Epoch as u8;
        let mut builder = Blake2HashBuilder::new()
            .update(preamble)
            .update(self.previous.as_bytes())
            .update(self.epoch.to_be_bytes());

        // Sort representatives by amount (and by public key as tiebreaker for stability)
        let mut sorted_representatives = self.representatives.clone();
        sorted_representatives.sort_by(|(pk1, amount1), (pk2, amount2)| {
            amount1
                .cmp(amount2)
                .then_with(|| pk1.as_bytes().cmp(pk2.as_bytes()))
        });

        for (public_key, amount) in &sorted_representatives {
            builder = builder
                .update(public_key.as_bytes())
                .update(amount.to_be_bytes());
        }
        builder.build()
    }
}

pub struct EpochBlockArgs {
    pub previous: BlockHash,
    pub epoch: u128,
    pub representatives: Vec<(PublicKey, Amount)>,
}

impl From<EpochBlockArgs> for EpochBlock {
    fn from(value: EpochBlockArgs) -> Self {
        let hashables = EpochHashables {
            previous: value.previous,
            epoch: value.epoch,
            representatives: value.representatives,
        };

        let hash = hashables.hash();

        Self { hashables, hash }
    }
}

impl From<EpochBlockArgs> for Block {
    fn from(value: EpochBlockArgs) -> Self {
        Block::Epoch(value.into())
    }
}

#[derive(PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct JsonEpochBlock {
    pub previous: BlockHash,
    pub epoch: u128,
    pub representatives: Vec<(Account, Amount)>,
}

impl From<JsonEpochBlock> for EpochBlock {
    fn from(value: JsonEpochBlock) -> Self {
        let hashables = EpochHashables {
            previous: value.previous,
            epoch: value.epoch,
            representatives: value
                .representatives
                .into_iter()
                .map(|(account, amount)| (account.into(), amount))
                .collect(),
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
        let block1: EpochBlock = EpochBlockArgs {
            previous: BlockHash::from(1),
            epoch: 42,
            representatives: vec![(789.into(), 420.into()), (123.into(), 456.into())],
        }
        .into();
        let mut buffer = Vec::new();
        block1.serialize_without_block_type(&mut buffer).unwrap();
        assert_eq!(
            EpochBlock::serialized_size(2),
            buffer.len(),
            "Serialized size should match"
        );

        let block2 = EpochBlock::deserialize(&mut buffer.as_slice()).unwrap();
        assert_eq!(block1, block2);
    }

    #[test]
    fn representatives() {
        let representatives = vec![(789.into(), 420.into()), (123.into(), 456.into())];
        let block: EpochBlock = EpochBlockArgs {
            previous: BlockHash::from(1),
            epoch: 42,
            representatives: representatives.clone(),
        }
        .into();
        assert_eq!(block.representatives(), representatives.as_slice());
    }

    #[test]
    fn epoch() {
        let epoch = 12345;
        let block: EpochBlock = EpochBlockArgs {
            previous: BlockHash::from(1),
            epoch,
            representatives: vec![(789.into(), 420.into())],
        }
        .into();
        assert_eq!(block.epoch(), epoch);
    }
}
