use bitvec::prelude::BitArray;
use rsnano_types::{BlockHash, DeserializationError, QualifiedRoot, Root, read_u64_be};

use crate::MessageVariant;

const FIXED_SIZE: usize = 1 + 8 + 1 + BlockHash::SERIALIZED_SIZE * 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClosePayloadKind {
    Request,
    UnknownBase,
    RecordDelta {
        upserts: Vec<(QualifiedRoot, BlockHash)>,
        removals: Vec<QualifiedRoot>,
    },
    CutDelta {
        additions: Vec<BlockHash>,
        removals: Vec<BlockHash>,
    },
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
        let tag = match &self.kind {
            ClosePayloadKind::Request => 0,
            ClosePayloadKind::UnknownBase => 1,
            ClosePayloadKind::RecordDelta { .. } => 2,
            ClosePayloadKind::SlotRequest => 4,
            ClosePayloadKind::ReportRequest => 5,
            ClosePayloadKind::CutDelta { .. } => 6,
        };
        writer.write_all(&[tag])?;
        writer.write_all(&self.epoch.to_be_bytes())?;
        writer.write_all(&[self.election_kind])?;
        writer.write_all(self.base.as_bytes())?;
        writer.write_all(self.target.as_bytes())?;
        match &self.kind {
            ClosePayloadKind::RecordDelta { upserts, removals } => {
                writer.write_all(&(upserts.len() as u32).to_be_bytes())?;
                writer.write_all(&(removals.len() as u32).to_be_bytes())?;
                for (root, hash) in upserts {
                    serialize_root(writer, root)?;
                    writer.write_all(hash.as_bytes())?;
                }
                for root in removals {
                    serialize_root(writer, root)?;
                }
            }
            ClosePayloadKind::CutDelta {
                additions,
                removals,
            } => {
                writer.write_all(&(additions.len() as u32).to_be_bytes())?;
                writer.write_all(&(removals.len() as u32).to_be_bytes())?;
                for hash in additions.iter().chain(removals) {
                    writer.write_all(hash.as_bytes())?;
                }
            }
            _ => {}
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
        let (first_count, second_count) = if tag == 2 || tag == 6 {
            if bytes.len() < 8 {
                return Err(DeserializationError::InvalidData);
            }
            let first = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
            let second = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
            bytes = &bytes[8..];
            (first, second)
        } else {
            (0, 0)
        };
        const ENTRY_SIZE: usize =
            Root::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 8 + BlockHash::SERIALIZED_SIZE;
        const ROOT_SIZE: usize = Root::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 8;
        let expected = match tag {
            2 => first_count * ENTRY_SIZE + second_count * ROOT_SIZE,
            6 => (first_count + second_count) * BlockHash::SERIALIZED_SIZE,
            _ => 0,
        };
        if bytes.len() != expected {
            return Err(DeserializationError::InvalidData);
        }
        let mut additions = Vec::with_capacity(first_count);
        let mut hash_removals = Vec::with_capacity(second_count);
        if tag == 6 {
            for _ in 0..first_count {
                additions.push(BlockHash::deserialize(&mut bytes)?);
            }
            for _ in 0..second_count {
                hash_removals.push(BlockHash::deserialize(&mut bytes)?);
            }
        }
        let mut upserts = Vec::with_capacity(first_count);
        let mut root_removals = Vec::with_capacity(second_count);
        if tag == 2 {
            for _ in 0..first_count {
                let root = deserialize_root(&mut bytes)?;
                let hash = BlockHash::deserialize(&mut bytes)?;
                upserts.push((root, hash));
            }
            for _ in 0..second_count {
                root_removals.push(deserialize_root(&mut bytes)?);
            }
        }
        let kind = match tag {
            0 => ClosePayloadKind::Request,
            1 => ClosePayloadKind::UnknownBase,
            2 => ClosePayloadKind::RecordDelta {
                upserts,
                removals: root_removals,
            },
            4 => ClosePayloadKind::SlotRequest,
            5 => ClosePayloadKind::ReportRequest,
            6 => ClosePayloadKind::CutDelta {
                additions,
                removals: hash_removals,
            },
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

fn serialize_root<T: std::io::Write>(writer: &mut T, root: &QualifiedRoot) -> std::io::Result<()> {
    writer.write_all(root.root.as_bytes())?;
    writer.write_all(root.previous.as_bytes())?;
    writer.write_all(&root.epoch.to_be_bytes())
}

fn deserialize_root(bytes: &mut &[u8]) -> Result<QualifiedRoot, DeserializationError> {
    let root = Root::deserialize(bytes)?;
    let previous = BlockHash::deserialize(bytes)?;
    let epoch = read_u64_be(bytes)?;
    Ok(QualifiedRoot::new(root, previous).with_epoch(epoch))
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
            kind: ClosePayloadKind::RecordDelta {
                upserts: vec![(
                    QualifiedRoot::new_test_instance().with_epoch(3),
                    BlockHash::from(4),
                )],
                removals: vec![QualifiedRoot::new_test_instance().with_epoch(4)],
            },
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

    #[test]
    fn roundtrip_cut_hash_delta() {
        let payload = ClosePayload {
            epoch: 7,
            election_kind: 0,
            base: BlockHash::from(1),
            target: BlockHash::from(2),
            kind: ClosePayloadKind::CutDelta {
                additions: vec![BlockHash::from(3), BlockHash::from(4)],
                removals: vec![BlockHash::from(5)],
            },
        };
        assert_deserializable(&Message::ClosePayload(payload));
    }
}
