use crate::{utils::{BufferWriter, FixedSizeSerialize, Stream, Serialize}, Account, Amount, BlockBase, BlockHash, BlockType, JsonBlock, Link, PublicKey, Root, Signature, WorkNonce};
use anyhow::Result;

#[derive(Clone, Debug)]
pub struct OrderingBlock {
    pub epoch: u64,
    pub committed_blocks: Vec<BlockHash>, 
    pub signature: Signature,
}

impl OrderingBlock {
    pub fn new(epoch: u64, committed_blocks: Vec<BlockHash>) -> Self {
        Self {
            epoch,
            committed_blocks,
            signature: Signature::default(),
        }
    }

    pub fn serialized_size() -> usize {
        std::mem::size_of::<u64>() // Epoch
            + 1000 * BlockHash::serialized_size() // fix hardcoded value
    }

    pub fn deserialize(_stream: &mut dyn Stream) -> Result<Self> {
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
        // Create a hash based on epoch and committed blocks
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        self.epoch.hash(&mut hasher);
        for block_hash in &self.committed_blocks {
            block_hash.hash(&mut hasher);
        }
        
        BlockHash::from(hasher.finish())
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

    fn set_work(&mut self, _work: WorkNonce) {
        // Ordering blocks don't need work
    }

    fn work(&self) -> WorkNonce {
        WorkNonce::default()
    }

    fn previous(&self) -> BlockHash {
        unimplemented!()
    }

    fn serialize_without_block_type(&self, writer: &mut dyn BufferWriter) {
        // Serialize epoch
        writer.write_u64_be_safe(self.epoch);
        
        // Serialize committed blocks count
        writer.write_u32_be_safe(self.committed_blocks.len() as u32);
        
        // Serialize each committed block hash
        for block_hash in &self.committed_blocks {
            block_hash.serialize(writer);
        }
    }

    fn root(&self) -> Root {
        // For ordering blocks, the root could be derived from the epoch
        Root::from(self.epoch)
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
        self.epoch == other.epoch && self.committed_blocks == other.committed_blocks
    }
}

impl Eq for OrderingBlock {}