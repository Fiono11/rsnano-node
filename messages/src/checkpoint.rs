use crate::{MessageVariant, ReconReply};
use bitvec::prelude::BitArray;
use rsnano_types::{BlockHash, ConsensusEpoch, DeserializationError};

/// A page of the canonical difference from the installed predecessor to d_e.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointReq {
    pub epoch: ConsensusEpoch,
    pub source: BlockHash,
    pub target: BlockHash,
    pub offset: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointReply {
    pub offset: u32,
    pub total: u32,
    pub difference: ReconReply,
}

impl CheckpointReq {
    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        self.source.serialize(writer)?;
        self.target.serialize(writer)?;
        writer.write_all(&self.offset.to_le_bytes())
    }
    pub const fn serialized_size(_: BitArray<u16>) -> usize {
        76
    }
    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let epoch = ConsensusEpoch::deserialize(&mut bytes)?;
        let source = BlockHash::deserialize(&mut bytes)?;
        let target = BlockHash::deserialize(&mut bytes)?;
        let offset = take_u32(&mut bytes)?;
        if !bytes.is_empty() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            epoch,
            source,
            target,
            offset,
        })
    }
}
impl MessageVariant for CheckpointReq {}

impl CheckpointReply {
    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(&self.offset.to_le_bytes())?;
        writer.write_all(&self.total.to_le_bytes())?;
        self.difference.serialize(writer)
    }
    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }
    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let offset = take_u32(&mut bytes)?;
        let total = take_u32(&mut bytes)?;
        let difference = ReconReply::deserialize_with_status(bytes, 3)?;
        let count = difference.added.len() + difference.removed.len();
        if count > ReconReply::MAX_ENTRIES || offset as u64 + count as u64 > total as u64 {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            offset,
            total,
            difference,
        })
    }
}
impl MessageVariant for CheckpointReply {
    fn header_extensions(&self, len: u16) -> BitArray<u16> {
        BitArray::new(len)
    }
}
fn take_u32(bytes: &mut &[u8]) -> Result<u32, DeserializationError> {
    if bytes.len() < 4 {
        return Err(DeserializationError::InvalidData);
    }
    let (head, rest) = bytes.split_at(4);
    *bytes = rest;
    Ok(u32::from_le_bytes(head.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};
    #[test]
    fn pages_roundtrip() {
        let difference = ReconReply::new_test_instance();
        assert_deserializable(&Message::CheckpointReq(CheckpointReq {
            epoch: difference.epoch,
            source: difference.source,
            target: difference.target,
            offset: 500,
        }));
        assert_deserializable(&Message::CheckpointReply(CheckpointReply {
            offset: 500,
            total: 502,
            difference,
        }));
    }
    #[test]
    fn page_past_end_is_rejected() {
        let page = CheckpointReply {
            offset: u32::MAX,
            total: u32::MAX,
            difference: ReconReply::new_test_instance(),
        };
        let mut bytes = Vec::new();
        page.serialize(&mut bytes).unwrap();
        assert!(CheckpointReply::deserialize(&bytes).is_err());
    }

    #[test]
    fn checkpoint_lock_tags_roundtrip_but_are_not_report_tags() {
        for status in [2, 3] {
            let mut difference = ReconReply::new_test_instance();
            difference.added[0].status = status;
            let mut report_bytes = Vec::new();
            difference.serialize(&mut report_bytes).unwrap();
            assert!(ReconReply::deserialize(&report_bytes).is_err());
            assert_deserializable(&Message::CheckpointReply(CheckpointReply {
                offset: 0,
                total: 2,
                difference,
            }));
        }
    }
}
