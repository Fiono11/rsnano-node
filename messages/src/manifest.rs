use bitvec::prelude::BitArray;

use rsnano_types::{BlockHash, ConsensusEpoch, DeserializationError};

use crate::MessageVariant;

/// RAI, "Immutable candidate inputs": one entry of a candidate's evidence
/// manifest. For one block in one epoch, which members of that epoch's
/// committee the leader's manifest names as first voters, as final voters
/// and as late notarizers, as bit sets over the committee's members in
/// canonical key order. Late notarizations enter only exclusion witnesses.
/// `settled` names the first voters whose first vote was settled, cast after
/// installing the epoch's predecessor checkpoint: only these form a fast
/// certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ManifestEntry {
    pub epoch: ConsensusEpoch,
    pub hash: BlockHash,
    pub first: u64,
    pub final_: u64,
    pub late: u64,
    pub settled: u64,
}

impl ManifestEntry {
    pub const SERIALIZED_SIZE: usize =
        ConsensusEpoch::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 32;

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        self.hash.serialize(writer)?;
        writer.write_all(&self.first.to_le_bytes())?;
        writer.write_all(&self.final_.to_le_bytes())?;
        writer.write_all(&self.late.to_le_bytes())?;
        writer.write_all(&self.settled.to_le_bytes())
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        Ok(Self {
            epoch: ConsensusEpoch::deserialize(bytes)?,
            hash: BlockHash::deserialize(bytes)?,
            first: u64::from_le_bytes(take::<8>(bytes)?),
            final_: u64::from_le_bytes(take::<8>(bytes)?),
            late: u64::from_le_bytes(take::<8>(bytes)?),
            settled: u64::from_le_bytes(take::<8>(bytes)?),
        })
    }
}

/// RAI: a request for the entries `from ..` of the evidence manifest with
/// the given digest, which an epoch proposal committed to. Any node holding
/// the manifest answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestReq {
    pub epoch: ConsensusEpoch,
    pub manifest: BlockHash,
    pub from: u32,
}

impl ManifestReq {
    pub const SERIALIZED_SIZE: usize =
        ConsensusEpoch::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 4;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(3),
            manifest: BlockHash::from(7),
            from: 1000,
        }
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        self.manifest.serialize(writer)?;
        writer.write_all(&self.from.to_le_bytes())
    }

    pub const fn serialized_size(_extensions: BitArray<u16>) -> usize {
        Self::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let manifest = BlockHash::deserialize(bytes)?;
        let from = u32::from_le_bytes(take::<4>(bytes)?);
        if !bytes.is_empty() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            epoch,
            manifest,
            from,
        })
    }
}

impl MessageVariant for ManifestReq {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        BitArray::ZERO
    }
}

/// RAI: one chunk of an evidence manifest: the entries `from .. from + n`
/// of `total`, in the manifest's canonical order
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestReply {
    pub epoch: ConsensusEpoch,
    pub manifest: BlockHash,
    pub total: u32,
    pub from: u32,
    pub entries: Vec<ManifestEntry>,
}

impl ManifestReply {
    pub const MAX_ENTRIES: usize = 1000;
    const HEAD: usize = ConsensusEpoch::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 8;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(3),
            manifest: BlockHash::from(7),
            total: 3,
            from: 1,
            entries: vec![
                ManifestEntry {
                    epoch: ConsensusEpoch::new(2),
                    hash: BlockHash::from(8),
                    first: 0b101,
                    final_: 0b1,
                    late: 0b10,
                    settled: 0b100,
                },
                ManifestEntry {
                    epoch: ConsensusEpoch::new(3),
                    hash: BlockHash::from(9),
                    first: 0b111,
                    final_: 0,
                    late: 0,
                    settled: 0b11,
                },
            ],
        }
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        self.manifest.serialize(writer)?;
        writer.write_all(&self.total.to_le_bytes())?;
        writer.write_all(&self.from.to_le_bytes())?;
        for entry in &self.entries {
            entry.serialize(writer)?;
        }
        Ok(())
    }

    /// The payload length is carried in the extensions
    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let manifest = BlockHash::deserialize(bytes)?;
        let total = u32::from_le_bytes(take::<4>(bytes)?);
        let from = u32::from_le_bytes(take::<4>(bytes)?);
        if bytes.len() % ManifestEntry::SERIALIZED_SIZE != 0
            || bytes.len() / ManifestEntry::SERIALIZED_SIZE > Self::MAX_ENTRIES
        {
            return Err(DeserializationError::InvalidData);
        }
        let mut entries = Vec::with_capacity(bytes.len() / ManifestEntry::SERIALIZED_SIZE);
        while !bytes.is_empty() {
            entries.push(ManifestEntry::deserialize(bytes)?);
        }
        Ok(Self {
            epoch,
            manifest,
            total,
            from,
            entries,
        })
    }
}

impl MessageVariant for ManifestReply {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        BitArray::new(
            (Self::HEAD
                + self.entries.len().min(Self::MAX_ENTRIES) * ManifestEntry::SERIALIZED_SIZE)
                as u16,
        )
    }
}

fn take<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N], DeserializationError> {
    if bytes.len() < N {
        return Err(DeserializationError::InvalidData);
    }
    let (head, tail) = bytes.split_at(N);
    *bytes = tail;
    let mut result = [0; N];
    result.copy_from_slice(head);
    Ok(result)
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn serialize_a_manifest_request() {
        assert_deserializable(&Message::ManifestReq(ManifestReq::new_test_instance()));
    }

    #[test]
    fn serialize_a_manifest_reply() {
        assert_deserializable(&Message::ManifestReply(ManifestReply::new_test_instance()));
    }
}
