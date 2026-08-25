use bitvec::prelude::BitArray;
use rsnano_types::{BlockHash, DeserializationError, QualifiedRoot, Root, read_u64_be};

use crate::MessageVariant;

const FIXED_SIZE: usize = 1 + 8 + 1 + BlockHash::SERIALIZED_SIZE * 2 + 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClosePayloadKind {
    Request,
    UnknownBase,
    Delta(Vec<(QualifiedRoot, BlockHash)>),
    DeltaTooLarge,
    SlotRequest,
    ReportRequest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosePayload {
    pub epoch: u64,
    pub election_kind: u8,
    pub base: BlockHash,
    pub target: BlockHash,
    pub kind: ClosePayloadKind,
}

impl ClosePayload {
    pub fn request(epoch: u64, election_kind: u8, base: BlockHash, target: BlockHash) -> Self {
        Self {
            epoch,
            election_kind,
            base,
            target,
            kind: ClosePayloadKind::Request,
        }
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        let (tag, additions): (u8, &[(QualifiedRoot, BlockHash)]) = match &self.kind {
            ClosePayloadKind::Request => (0, &[]),
            ClosePayloadKind::UnknownBase => (1, &[]),
            ClosePayloadKind::Delta(additions) => (2, additions),
            ClosePayloadKind::DeltaTooLarge => (3, &[]),
            ClosePayloadKind::SlotRequest => (4, &[]),
            ClosePayloadKind::ReportRequest => (5, &[]),
        };
        writer.write_all(&[tag])?;
        writer.write_all(&self.epoch.to_be_bytes())?;
        writer.write_all(&[self.election_kind])?;
        writer.write_all(self.base.as_bytes())?;
        writer.write_all(self.target.as_bytes())?;
        writer.write_all(&(additions.len() as u16).to_be_bytes())?;
        for (root, hash) in additions {
            writer.write_all(root.root.as_bytes())?;
            writer.write_all(root.previous.as_bytes())?;
            writer.write_all(&root.epoch.to_be_bytes())?;
            writer.write_all(hash.as_bytes())?;
        }
        Ok(())
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        if bytes.len() < FIXED_SIZE {
            return Err(DeserializationError::InvalidData);
        }
        let tag = bytes[0];
        bytes = &bytes[1..];
        let epoch = read_u64_be(&mut bytes)?;
        let election_kind = bytes[0];
        bytes = &bytes[1..];
        let base = BlockHash::deserialize(&mut bytes)?;
        let target = BlockHash::deserialize(&mut bytes)?;
        let count = u16::from_be_bytes(bytes[..2].try_into().unwrap()) as usize;
        bytes = &bytes[2..];
        const ENTRY_SIZE: usize =
            Root::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 8 + BlockHash::SERIALIZED_SIZE;
        if bytes.len() != count * ENTRY_SIZE || tag != 2 && count != 0 {
            return Err(DeserializationError::InvalidData);
        }
        let mut additions = Vec::with_capacity(count);
        for _ in 0..count {
            let root = Root::deserialize(&mut bytes)?;
            let previous = BlockHash::deserialize(&mut bytes)?;
            let root_epoch = read_u64_be(&mut bytes)?;
            let hash = BlockHash::deserialize(&mut bytes)?;
            additions.push((
                QualifiedRoot::new(root, previous).with_epoch(root_epoch),
                hash,
            ));
        }
        let kind = match tag {
            0 => ClosePayloadKind::Request,
            1 => ClosePayloadKind::UnknownBase,
            2 => ClosePayloadKind::Delta(additions),
            3 => ClosePayloadKind::DeltaTooLarge,
            4 => ClosePayloadKind::SlotRequest,
            5 => ClosePayloadKind::ReportRequest,
            _ => return Err(DeserializationError::InvalidData),
        };
        Ok(Self {
            epoch,
            election_kind,
            base,
            target,
            kind,
        })
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }
}

impl MessageVariant for ClosePayload {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn roundtrip_delta() {
        let payload = ClosePayload {
            epoch: 3,
            election_kind: 1,
            base: BlockHash::from(1),
            target: BlockHash::from(2),
            kind: ClosePayloadKind::Delta(vec![(
                QualifiedRoot::new_test_instance().with_epoch(3),
                BlockHash::from(4),
            )]),
        };
        assert_deserializable(&Message::ClosePayload(payload));
    }

    #[test]
    fn roundtrip_report_request() {
        let payload = ClosePayload {
            epoch: 7,
            election_kind: 0,
            base: BlockHash::ZERO,
            target: BlockHash::ZERO,
            kind: ClosePayloadKind::ReportRequest,
        };
        assert_deserializable(&Message::ClosePayload(payload));
    }
}
