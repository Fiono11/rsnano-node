use bitvec::prelude::BitArray;

use rsnano_types::{BlockHash, ConsensusEpoch, DeserializationError};

use crate::MessageVariant;

/// RAI, "Reconstructing a report": a request for the retained signed votes
/// behind the tagged entries of an epoch that a requester can not justify
/// from the votes it holds. A replica answers with the original signed vote
/// batches it retained for those hashes, as certificate evidence; the
/// requester assembles the certificates itself. Nothing but signed votes is
/// relayed: a reply is never a verdict on a status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceReq {
    pub epoch: ConsensusEpoch,
    pub hashes: Vec<BlockHash>,
}

impl EvidenceReq {
    /// Hashes in one request: 32 KiB on the wire
    pub const MAX_HASHES: usize = 1024;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            hashes: vec![BlockHash::from(2), BlockHash::from(3)],
        }
    }

    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        for hash in &self.hashes {
            hash.serialize(writer)?;
        }
        Ok(())
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let mut hashes = Vec::new();
        while !bytes.is_empty() {
            if hashes.len() >= Self::MAX_HASHES {
                return Err(DeserializationError::InvalidData);
            }
            hashes.push(BlockHash::deserialize(bytes)?);
        }
        if hashes.is_empty() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self { epoch, hashes })
    }
}

impl MessageVariant for EvidenceReq {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn serialize_evidence_req() {
        assert_deserializable(&Message::EvidenceReq(EvidenceReq::new_test_instance()));
    }

    #[test]
    fn an_evidence_req_names_at_least_one_hash() {
        let mut bytes = Vec::new();
        EvidenceReq {
            epoch: ConsensusEpoch::new(1),
            hashes: Vec::new(),
        }
        .serialize(&mut bytes)
        .unwrap();
        assert!(EvidenceReq::deserialize(&bytes).is_err());
    }
}
