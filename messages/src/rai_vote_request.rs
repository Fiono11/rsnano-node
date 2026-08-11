use std::io::Write;

use bitvec::prelude::BitArray;
use rsnano_types::{
    Account, BlockHash, DeserializationError, QualifiedRoot, RaiEpoch, RaiSlotId, Root,
    read_u32_be, read_u64_be,
};

use crate::MessageVariant;

/// Marks a `RaiVoteRequest` as synthetic close-certificate repair rather than
/// slot repair. `sequence` is sender-local, so reserving its high bit keeps the
/// wire format backward compatible while making request classification O(1).
pub const RAI_CLOSE_REPAIR_SEQUENCE_FLAG: u64 = 1 << 63;
/// Marks an ordinary slot/payload repair request. Together with
/// `RAI_CLOSE_REPAIR_SEQUENCE_FLAG`, this lets current peers avoid the legacy
/// synthetic-root classification fallback even for zero-hash payload repair.
pub const RAI_SLOT_REPAIR_SEQUENCE_FLAG: u64 = 1 << 62;
/// Bits available to the sender-local monotonically increasing counter.
pub const RAI_REPAIR_SEQUENCE_COUNTER_MASK: u64 =
    !(RAI_CLOSE_REPAIR_SEQUENCE_FLAG | RAI_SLOT_REPAIR_SEQUENCE_FLAG);

/// Requests a missing RAI payload/preimage for one election.
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
    /// A canonical close preimage returned by a repair peer. Exact close
    /// requests name its nonzero digest and leave this empty; replies carry
    /// only the candidate needed to validate already-retained signed leaves.
    pub close_version: Option<RaiCloseVersionWire>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiCloseVersionWire {
    Cut(RaiCloseCutWire),
    Record(RaiCloseRecordWire),
    /// One canonical fragment of a cut preimage which does not fit in a
    /// single Nano wire frame.
    CutChunk(RaiCloseCutChunkWire),
    /// One canonical fragment of a close-record preimage which does not fit
    /// in a single Nano wire frame.
    RecordChunk(RaiCloseRecordChunkWire),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseCutChunkWire {
    pub epoch: u64,
    pub chunk_index: u32,
    pub chunk_count: u32,
    pub obligations: Vec<RaiSlotId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCloseRecordChunkWire {
    pub epoch: u64,
    pub previous: BlockHash,
    pub chunk_index: u32,
    pub chunk_count: u32,
    pub frontiers: Vec<(Account, u64, BlockHash)>,
}

const RAI_VOTE_REQUEST_FIXED_SIZE: usize = 8 + 8 + 32 + 32;
const RAI_CLOSE_ENTRY_SIZE: usize = 8 + 64;
const CUT_CHUNK_FIXED_SIZE: usize = RAI_VOTE_REQUEST_FIXED_SIZE + 1 + 8 + 4 + 4;
const RECORD_CHUNK_FIXED_SIZE: usize = CUT_CHUNK_FIXED_SIZE + 32;

/// Maximum number of cut entries in a chunk while keeping the complete
/// RaiVoteRequest payload representable by the message header's `u16` length.
pub const MAX_RAI_CLOSE_CUT_CHUNK_ENTRIES: usize =
    (u16::MAX as usize - CUT_CHUNK_FIXED_SIZE) / RAI_CLOSE_ENTRY_SIZE;

/// Maximum number of close-record entries in a chunk while keeping the
/// complete RaiVoteRequest payload representable by the message header's
/// `u16` length.
pub const MAX_RAI_CLOSE_RECORD_CHUNK_ENTRIES: usize =
    (u16::MAX as usize - RECORD_CHUNK_FIXED_SIZE) / RAI_CLOSE_ENTRY_SIZE;

/// Maximum number of fragments retained for one close-repair preimage.
/// This bounds reassembly memory even when a peer advertises an arbitrarily
/// large `chunk_count` and keeps the partial assembly alive with new chunks.
pub const MAX_RAI_CLOSE_CHUNKS: u32 = 64;

fn valid_chunk_layout(
    chunk_index: u32,
    chunk_count: u32,
    entries: usize,
    max_entries: usize,
) -> bool {
    chunk_count > 1
        && chunk_count <= MAX_RAI_CLOSE_CHUNKS
        && chunk_index < chunk_count
        && entries > 0
        && entries <= max_entries
        && (chunk_index + 1 == chunk_count || entries == max_entries)
}

impl RaiCloseCutChunkWire {
    pub fn has_valid_layout(&self) -> bool {
        valid_chunk_layout(
            self.chunk_index,
            self.chunk_count,
            self.obligations.len(),
            MAX_RAI_CLOSE_CUT_CHUNK_ENTRIES,
        )
    }
}

impl RaiCloseRecordChunkWire {
    pub fn has_valid_layout(&self) -> bool {
        valid_chunk_layout(
            self.chunk_index,
            self.chunk_count,
            self.frontiers.len(),
            MAX_RAI_CLOSE_RECORD_CHUNK_ENTRIES,
        )
    }
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
                RaiCloseVersionWire::CutChunk(chunk) => {
                    writer.write_all(&[3])?;
                    writer.write_all(&chunk.epoch.to_be_bytes())?;
                    writer.write_all(&chunk.chunk_index.to_be_bytes())?;
                    writer.write_all(&chunk.chunk_count.to_be_bytes())?;
                    for slot in &chunk.obligations {
                        writer.write_all(&slot.epoch.number().to_be_bytes())?;
                        writer.write_all(&slot.root.to_bytes())?;
                    }
                }
                RaiCloseVersionWire::RecordChunk(chunk) => {
                    writer.write_all(&[4])?;
                    writer.write_all(&chunk.epoch.to_be_bytes())?;
                    writer.write_all(chunk.previous.as_bytes())?;
                    writer.write_all(&chunk.chunk_index.to_be_bytes())?;
                    writer.write_all(&chunk.chunk_count.to_be_bytes())?;
                    for (account, height, frontier) in &chunk.frontiers {
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
                3 => {
                    let chunk_index = read_u32_be(&mut bytes)?;
                    let chunk_count = read_u32_be(&mut bytes)?;
                    if bytes.len() % RAI_CLOSE_ENTRY_SIZE != 0 {
                        return Err(DeserializationError::InvalidData);
                    }
                    let mut obligations = Vec::new();
                    while !bytes.is_empty() {
                        obligations.push(RaiSlotId {
                            epoch: RaiEpoch::new(read_u64_be(&mut bytes)?),
                            root: QualifiedRoot::deserialize(&mut bytes)?,
                        });
                    }
                    let chunk = RaiCloseCutChunkWire {
                        epoch: version_epoch,
                        chunk_index,
                        chunk_count,
                        obligations,
                    };
                    if !chunk.has_valid_layout() {
                        return Err(DeserializationError::InvalidData);
                    }
                    Some(RaiCloseVersionWire::CutChunk(chunk))
                }
                4 => {
                    let previous = BlockHash::deserialize(&mut bytes)?;
                    let chunk_index = read_u32_be(&mut bytes)?;
                    let chunk_count = read_u32_be(&mut bytes)?;
                    if bytes.len() % RAI_CLOSE_ENTRY_SIZE != 0 {
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
                    let chunk = RaiCloseRecordChunkWire {
                        epoch: version_epoch,
                        previous,
                        chunk_index,
                        chunk_count,
                        frontiers,
                    };
                    if !chunk.has_valid_layout() {
                        return Err(DeserializationError::InvalidData);
                    }
                    Some(RaiCloseVersionWire::RecordChunk(chunk))
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

    /// Splits an oversized close preimage into canonical, independently
    /// framed repair messages. Small preimages retain the original wire form.
    pub fn into_chunks(self) -> Vec<Self> {
        let Self {
            sequence,
            epoch,
            hash,
            root,
            close_version,
        } = self;

        let versions = match close_version {
            Some(RaiCloseVersionWire::Cut(cut))
                if cut.obligations.len() > MAX_RAI_CLOSE_CUT_CHUNK_ENTRIES =>
            {
                let chunk_count = cut
                    .obligations
                    .len()
                    .div_ceil(MAX_RAI_CLOSE_CUT_CHUNK_ENTRIES);
                let chunk_count =
                    u32::try_from(chunk_count).expect("RAI close cut has too many chunks");
                cut.obligations
                    .chunks(MAX_RAI_CLOSE_CUT_CHUNK_ENTRIES)
                    .enumerate()
                    .map(|(chunk_index, obligations)| {
                        RaiCloseVersionWire::CutChunk(RaiCloseCutChunkWire {
                            epoch: cut.epoch,
                            chunk_index: chunk_index as u32,
                            chunk_count,
                            obligations: obligations.to_vec(),
                        })
                    })
                    .collect()
            }
            Some(RaiCloseVersionWire::Record(record))
                if record.frontiers.len() > MAX_RAI_CLOSE_RECORD_CHUNK_ENTRIES =>
            {
                let chunk_count = record
                    .frontiers
                    .len()
                    .div_ceil(MAX_RAI_CLOSE_RECORD_CHUNK_ENTRIES);
                let chunk_count =
                    u32::try_from(chunk_count).expect("RAI close record has too many chunks");
                record
                    .frontiers
                    .chunks(MAX_RAI_CLOSE_RECORD_CHUNK_ENTRIES)
                    .enumerate()
                    .map(|(chunk_index, frontiers)| {
                        RaiCloseVersionWire::RecordChunk(RaiCloseRecordChunkWire {
                            epoch: record.epoch,
                            previous: record.previous,
                            chunk_index: chunk_index as u32,
                            chunk_count,
                            frontiers: frontiers.to_vec(),
                        })
                    })
                    .collect()
            }
            Some(version) => vec![version],
            None => vec![],
        };

        if versions.is_empty() {
            return vec![Self {
                sequence,
                epoch,
                hash,
                root,
                close_version: None,
            }];
        }

        versions
            .into_iter()
            .map(|close_version| Self {
                sequence,
                epoch,
                hash,
                root,
                close_version: Some(close_version),
            })
            .collect()
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
    use crate::{Message, MessageHeader, MessageSerializer, assert_deserializable};

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

    #[test]
    fn oversized_close_cut_is_split_into_valid_frames() {
        let obligations = (0..=MAX_RAI_CLOSE_CUT_CHUNK_ENTRIES)
            .map(|i| RaiSlotId {
                epoch: RaiEpoch::new(8),
                root: QualifiedRoot::new(Root::from(i as u64 + 1), BlockHash::from(10)),
            })
            .collect::<Vec<_>>();
        let chunks = RaiVoteRequest {
            sequence: 44,
            epoch: 8,
            hash: BlockHash::from(7),
            root: Root::from(8),
            close_version: Some(RaiCloseVersionWire::Cut(RaiCloseCutWire {
                epoch: 8,
                obligations,
            })),
        }
        .into_chunks();

        assert_eq!(chunks.len(), 2);
        for chunk in chunks {
            let message = Message::RaiVoteRequest(chunk.clone());
            let mut serializer = MessageSerializer::default();
            let mut bytes = serializer.serialize(&message);
            let header = MessageHeader::deserialize(&mut bytes).unwrap();
            assert!(header.payload_length() <= u16::MAX as usize);
            assert_eq!(
                Message::deserialize(bytes, &header, 0).unwrap(),
                Message::RaiVoteRequest(chunk)
            );
        }
    }

    #[test]
    fn oversized_close_record_is_split_into_valid_frames() {
        let frontiers = (0..MAX_RAI_CLOSE_RECORD_CHUNK_ENTRIES * 2 + 1)
            .map(|i| (Account::from(i as u64 + 1), i as u64, BlockHash::from(6)))
            .collect::<Vec<_>>();
        let chunks = RaiVoteRequest {
            sequence: 45,
            epoch: 9,
            hash: BlockHash::from(1),
            root: Root::from(2),
            close_version: Some(RaiCloseVersionWire::Record(RaiCloseRecordWire {
                epoch: 9,
                previous: BlockHash::from(3),
                frontiers,
            })),
        }
        .into_chunks();

        assert_eq!(chunks.len(), 3);
        assert!(chunks.into_iter().all(|request| {
            matches!(
                request.close_version,
                Some(RaiCloseVersionWire::RecordChunk(chunk)) if chunk.has_valid_layout()
            )
        }));
    }

    #[test]
    fn rejects_noncanonical_close_chunk() {
        let request = RaiVoteRequest {
            sequence: 46,
            epoch: 8,
            hash: BlockHash::from(7),
            root: Root::from(8),
            close_version: Some(RaiCloseVersionWire::CutChunk(RaiCloseCutChunkWire {
                epoch: 8,
                chunk_index: 0,
                chunk_count: 2,
                obligations: vec![RaiSlotId {
                    epoch: RaiEpoch::new(8),
                    root: QualifiedRoot::new(Root::from(9), BlockHash::from(10)),
                }],
            })),
        };
        let mut bytes = Vec::new();
        request.serialize(&mut bytes).unwrap();

        assert!(matches!(
            RaiVoteRequest::deserialize(&bytes),
            Err(DeserializationError::InvalidData)
        ));
    }

    #[test]
    fn rejects_excessive_close_chunk_count() {
        let chunk = RaiCloseCutChunkWire {
            epoch: 8,
            chunk_index: MAX_RAI_CLOSE_CHUNKS,
            chunk_count: MAX_RAI_CLOSE_CHUNKS + 1,
            obligations: vec![RaiSlotId {
                epoch: RaiEpoch::new(8),
                root: QualifiedRoot::new(Root::from(9), BlockHash::from(10)),
            }],
        };
        assert!(!chunk.has_valid_layout());

        let request = RaiVoteRequest {
            sequence: 47,
            epoch: 8,
            hash: BlockHash::from(7),
            root: Root::from(8),
            close_version: Some(RaiCloseVersionWire::CutChunk(chunk)),
        };
        let mut bytes = Vec::new();
        request.serialize(&mut bytes).unwrap();

        assert!(matches!(
            RaiVoteRequest::deserialize(&bytes),
            Err(DeserializationError::InvalidData)
        ));
    }
}
