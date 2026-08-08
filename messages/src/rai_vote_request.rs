use std::io::Write;

use bitvec::prelude::BitArray;
use rsnano_types::{
    Account, BlockHash, DeserializationError, QualifiedRoot, RaiEpoch, RaiSlotId, Root, read_u64_be,
};

use crate::MessageVariant;

/// Requests repair votes for one RAI election.
///
/// `sequence` is sender-local and deliberately participates in the wire
/// payload so repeated requests cannot be collapsed by duplicate filtering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiVoteRequest {
    pub sequence: u64,
    /// The RAI epoch whose election is being repaired. Roots can be reused by
    /// later epochs, so root/hash alone is not a stable election identity.
    pub epoch: u64,
    pub hash: BlockHash,
    pub root: Root,
    /// A canonical close preimage returned by a repair peer. Requests leave
    /// this empty; replies carry the candidate needed to validate cached votes.
    pub close_version: Option<RaiCloseVersionWire>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiCloseVersionWire {
    Cut(RaiCloseCutWire),
    Record(RaiCloseRecordWire),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseCutWire {
    pub epoch: u64,
    pub obligations: Vec<RaiSlotId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseRecordWire {
    pub epoch: u64,
    pub previous: BlockHash,
    pub frontiers: Vec<(Account, u64, BlockHash)>,
}

impl RaiVoteRequest {
    pub fn serialize<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&self.sequence.to_be_bytes())?;
        writer.write_all(&self.epoch.to_be_bytes())?;
        writer.write_all(self.hash.as_bytes())?;
        writer.write_all(self.root.as_bytes())?;
        if let Some(version) = &self.close_version {
            match version {
                RaiCloseVersionWire::Cut(cut) => {
                    writer.write_all(&[1])?;
                    writer.write_all(&cut.epoch.to_be_bytes())?;
                    for slot in &cut.obligations {
                        writer.write_all(&slot.epoch.number().to_be_bytes())?;
                        writer.write_all(&slot.root.to_bytes())?;
                    }
                }
                RaiCloseVersionWire::Record(record) => {
                    writer.write_all(&[2])?;
                    writer.write_all(&record.epoch.to_be_bytes())?;
                    writer.write_all(record.previous.as_bytes())?;
                    for (account, height, frontier) in &record.frontiers {
                        writer.write_all(account.as_bytes())?;
                        writer.write_all(&height.to_be_bytes())?;
                        writer.write_all(frontier.as_bytes())?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let sequence = read_u64_be(&mut bytes)?;
        let epoch = read_u64_be(&mut bytes)?;
        let hash = BlockHash::deserialize(&mut bytes)?;
        let root = Root::deserialize(&mut bytes)?;
        let close_version = if bytes.is_empty() {
            None
        } else {
            let (&kind, rest) = bytes
                .split_first()
                .ok_or(DeserializationError::InvalidData)?;
            bytes = rest;
            let version_epoch = read_u64_be(&mut bytes)?;
            match kind {
                1 => {
                    if bytes.len() % 72 != 0 {
                        return Err(DeserializationError::InvalidData);
                    }
                    let mut obligations = Vec::new();
                    while !bytes.is_empty() {
                        obligations.push(RaiSlotId {
                            epoch: RaiEpoch::new(read_u64_be(&mut bytes)?),
                            root: QualifiedRoot::deserialize(&mut bytes)?,
                        });
                    }
                    Some(RaiCloseVersionWire::Cut(RaiCloseCutWire {
                        epoch: version_epoch,
                        obligations,
                    }))
                }
                2 => {
                    let previous = BlockHash::deserialize(&mut bytes)?;
                    if bytes.len() % 72 != 0 {
                        return Err(DeserializationError::InvalidData);
                    }
                    let mut frontiers = Vec::new();
                    while !bytes.is_empty() {
                        frontiers.push((
                            Account::deserialize(&mut bytes)?,
                            read_u64_be(&mut bytes)?,
                            BlockHash::deserialize(&mut bytes)?,
                        ));
                    }
                    Some(RaiCloseVersionWire::Record(RaiCloseRecordWire {
                        epoch: version_epoch,
                        previous,
                        frontiers,
                    }))
                }
                _ => return Err(DeserializationError::InvalidData),
            }
        };
        if hash.is_zero() && root.is_zero() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            sequence,
            epoch,
            hash,
            root,
            close_version,
        })
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }
}

impl MessageVariant for RaiVoteRequest {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn close_record_roundtrip() {
        assert_deserializable(&Message::RaiVoteRequest(RaiVoteRequest {
            sequence: 42,
            epoch: 7,
            hash: BlockHash::from(1),
            root: Root::from(2),
            close_version: Some(RaiCloseVersionWire::Record(RaiCloseRecordWire {
                epoch: 7,
                previous: BlockHash::from(3),
                frontiers: vec![(Account::from(4), 5, BlockHash::from(6))],
            })),
        }));
    }

    #[test]
    fn close_cut_roundtrip() {
        assert_deserializable(&Message::RaiVoteRequest(RaiVoteRequest {
            sequence: 43,
            epoch: 8,
            hash: BlockHash::from(7),
            root: Root::from(8),
            close_version: Some(RaiCloseVersionWire::Cut(RaiCloseCutWire {
                epoch: 8,
                obligations: vec![RaiSlotId {
                    epoch: RaiEpoch::new(8),
                    root: QualifiedRoot::new(Root::from(9), BlockHash::from(10)),
                }],
            })),
        }));
    }
}
