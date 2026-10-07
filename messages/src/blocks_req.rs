use bitvec::prelude::BitArray;

use rsnano_types::{BlockHash, DeserializationError};

use crate::MessageVariant;

/// RAI: a request for blocks by hash. A node that installed a checkpoint
/// finalizing blocks its ledger lacks asks for them; a node holding one
/// answers with the block as an evidence publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlocksReq {
    pub hashes: Vec<BlockHash>,
}

impl BlocksReq {
    pub const MAX_HASHES: usize = 255;

    pub fn new_test_instance() -> Self {
        Self {
            hashes: vec![BlockHash::from(1), BlockHash::from(2)],
        }
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        for hash in &self.hashes {
            hash.serialize(writer)?;
        }
        Ok(())
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        (extensions.data as usize & 0xFF) * BlockHash::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        if bytes.len() % BlockHash::SERIALIZED_SIZE != 0
            || bytes.len() / BlockHash::SERIALIZED_SIZE > Self::MAX_HASHES
        {
            return Err(DeserializationError::InvalidData);
        }
        let mut hashes = Vec::with_capacity(bytes.len() / BlockHash::SERIALIZED_SIZE);
        while !bytes.is_empty() {
            hashes.push(BlockHash::deserialize(bytes)?);
        }
        Ok(Self { hashes })
    }
}

impl MessageVariant for BlocksReq {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        BitArray::new(self.hashes.len().min(Self::MAX_HASHES) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn serialize_a_blocks_request() {
        assert_deserializable(&Message::BlocksReq(BlocksReq::new_test_instance()));
    }

    #[test]
    fn a_full_request_round_trips() {
        let request = BlocksReq {
            hashes: (0..BlocksReq::MAX_HASHES as u64)
                .map(BlockHash::from)
                .collect(),
        };
        assert_deserializable(&Message::BlocksReq(request));
    }
}
