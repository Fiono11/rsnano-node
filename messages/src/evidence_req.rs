use bitvec::prelude::BitArray;

use rsnano_types::{BlockHash, ConsensusEpoch, DeserializationError};

use crate::MessageVariant;

/// RAI: a request for the signed votes behind a report's certificates. A
/// node that could not justify some certified entries of a report from the
/// votes it holds asks for them; a node holding them relays the original
/// signed votes as certificate evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceReq {
    pub epoch: ConsensusEpoch,
    pub hashes: Vec<BlockHash>,
}

impl EvidenceReq {
    pub const MAX_HASHES: usize = 255;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(2),
            hashes: vec![BlockHash::from(1), BlockHash::from(2)],
        }
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        for hash in &self.hashes {
            hash.serialize(writer)?;
        }
        Ok(())
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        ConsensusEpoch::SERIALIZED_SIZE
            + (extensions.data as usize & 0xFF) * BlockHash::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        if bytes.len() % BlockHash::SERIALIZED_SIZE != 0
            || bytes.len() / BlockHash::SERIALIZED_SIZE > Self::MAX_HASHES
        {
            return Err(DeserializationError::InvalidData);
        }
        let mut hashes = Vec::with_capacity(bytes.len() / BlockHash::SERIALIZED_SIZE);
        while !bytes.is_empty() {
            hashes.push(BlockHash::deserialize(bytes)?);
        }
        Ok(Self { epoch, hashes })
    }
}

impl MessageVariant for EvidenceReq {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        BitArray::new(self.hashes.len().min(Self::MAX_HASHES) as u16)
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn serialize_an_evidence_request() {
        assert_deserializable(&Message::EvidenceReq(EvidenceReq::new_test_instance()));
    }
}
