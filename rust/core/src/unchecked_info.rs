use std::{
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use super::BlockHash;
use crate::{
    deserialize_block_enum, serialize_block_enum,
    utils::{Deserialize, MemoryStream, Serialize, Stream, StreamExt},
    BlockEnum,
};

/// Information on an unchecked block
#[derive(Default, Clone, Debug)]
pub struct UncheckedInfo {
    // todo: Remove Option as soon as no C++ code requires the empty constructor
    pub block: Option<Arc<RwLock<BlockEnum>>>,

    /// Seconds since posix epoch
    pub modified: u64,
}

impl UncheckedInfo {
    pub fn new(block: Arc<RwLock<BlockEnum>>) -> Self {
        Self {
            block: Some(block),
            modified: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }

    pub fn null() -> Self {
        Self {
            block: None,
            modified: 0,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut stream = MemoryStream::new();
        self.serialize(&mut stream).unwrap();
        stream.to_vec()
    }
}

impl Serialize for UncheckedInfo {
    fn serialized_size() -> usize {
        0 //todo remove
    }

    fn serialize(&self, stream: &mut dyn Stream) -> anyhow::Result<()> {
        serialize_block_enum(stream, &self.block.as_ref().unwrap().read().unwrap())?;
        stream.write_u64_ne(self.modified)
    }
}

impl Deserialize for UncheckedInfo {
    type Target = Self;

    fn deserialize(stream: &mut dyn Stream) -> anyhow::Result<Self::Target> {
        let block = deserialize_block_enum(stream)?;
        let modified = stream.read_u64_ne()?;
        Ok(Self {
            block: Some(Arc::new(RwLock::new(block))),
            modified,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UncheckedKey {
    pub previous: BlockHash,
    pub hash: BlockHash,
}

impl UncheckedKey {
    pub fn new(previous: BlockHash, hash: BlockHash) -> Self {
        Self {
            previous,
            hash,
        }
    }
    
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut result = [0; 64];
        result[..32].copy_from_slice(self.previous.as_bytes());
        result[32..].copy_from_slice(self.hash.as_bytes());
        result
    }
}

impl Deserialize for UncheckedKey {
    type Target = Self;

    fn deserialize(stream: &mut dyn Stream) -> anyhow::Result<Self::Target> {
        let previous = BlockHash::deserialize(stream)?;
        let hash = BlockHash::deserialize(stream)?;
        Ok(Self { previous, hash })
    }
}

impl Serialize for UncheckedKey {
    fn serialized_size() -> usize {
        BlockHash::serialized_size() * 2
    }

    fn serialize(&self, stream: &mut dyn Stream) -> anyhow::Result<()> {
        self.previous.serialize(stream)?;
        self.hash.serialize(stream)
    }
}
