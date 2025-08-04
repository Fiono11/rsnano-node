use crate::{utils::{BufferWriter, FixedSizeSerialize, Stream}, Account, Amount, BlockBase, BlockHash, BlockType, JsonBlock, Link, PublicKey, Root, Signature, WorkNonce};
use anyhow::Result;

#[derive(Clone, Debug)]
pub struct OrderingBlock {
    pub epoch: u64,
    pub committed_blocks: Vec<BlockHash>, 
    pub signature: Signature,
}

impl OrderingBlock {
    pub fn serialized_size() -> usize {
        std::mem::size_of::<u64>() // Epoch
            + 1000 * BlockHash::serialized_size() // fix hardcoded value
    }

    pub fn deserialize(stream: &mut dyn Stream) -> Result<Self> {
        unimplemented!()
    }
}

impl BlockBase for OrderingBlock {
    fn block_type(&self) -> BlockType {
        BlockType::Ordering
    }

    fn account_field(&self) -> Option<Account> {
        None
    }

    fn hash(&self) -> BlockHash {
        unimplemented!()
    }

    fn link_field(&self) -> Option<Link> {
        None
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn set_signature(&mut self, signature: Signature) {
        self.signature = signature;
    }

    fn set_work(&mut self, work: WorkNonce) {
        unimplemented!()
    }

    fn work(&self) -> WorkNonce {
        WorkNonce::default()
    }

    fn previous(&self) -> BlockHash {
        unimplemented!()
    }

    fn serialize_without_block_type(&self, writer: &mut dyn BufferWriter) {
        unimplemented!()
    }

    fn root(&self) -> Root {
        unimplemented!()
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
        unimplemented!()
    }
}

impl PartialEq for OrderingBlock {
    fn eq(&self, other: &Self) -> bool {
        unimplemented!()
    }
}

impl Eq for OrderingBlock {}