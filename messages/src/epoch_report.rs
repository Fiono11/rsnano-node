use bitvec::prelude::BitArray;
use rsnano_types::{
    Blake2Hash, Blake2HashBuilder, BlockHash, DeserializationError, PrivateKey, PublicKey, Root,
    Signature, SlotRoot,
};
use std::io::{Read, Write};

use crate::MessageVariant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochStart {
    pub epoch: u64,
    pub starts_at_unix_ms: u64,
    pub closes_at_unix_ms: u64,
}

impl EpochStart {
    pub const SERIALIZED_SIZE: usize = 24;

    pub fn serialize<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&self.epoch.to_be_bytes())?;
        writer.write_all(&self.starts_at_unix_ms.to_be_bytes())?;
        writer.write_all(&self.closes_at_unix_ms.to_be_bytes())
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        if bytes.len() != Self::SERIALIZED_SIZE {
            return Err(DeserializationError::InvalidData);
        }
        let mut value = [0; 8];
        bytes.read_exact(&mut value)?;
        let epoch = u64::from_be_bytes(value);
        bytes.read_exact(&mut value)?;
        let starts_at_unix_ms = u64::from_be_bytes(value);
        bytes.read_exact(&mut value)?;
        let closes_at_unix_ms = u64::from_be_bytes(value);
        if epoch == 0 || starts_at_unix_ms >= closes_at_unix_ms {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            epoch,
            starts_at_unix_ms,
            closes_at_unix_ms,
        })
    }
}

impl MessageVariant for EpochStart {}

/// A bounded part of one representative's epoch-close report.
///
/// Chunks are independently signed so they can be validated before the complete report is
/// assembled.  A report is delivered only after every chunk has arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochReportChunk {
    pub epoch: u64,
    pub reporter: PublicKey,
    pub chunk_index: u16,
    pub chunk_count: u16,
    pub elections: Vec<SlotRoot>,
    pub signature: Signature,
}

impl EpochReportChunk {
    const FIXED_SIZE: usize = 8 + 32 + 2 + 2 + 2 + 64;
    pub const MAX_ELECTIONS: usize = (u16::MAX as usize - Self::FIXED_SIZE)
        / (Root::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE);

    pub fn new(
        epoch: u64,
        reporter_key: &PrivateKey,
        chunk_index: u16,
        chunk_count: u16,
        mut elections: Vec<SlotRoot>,
    ) -> Self {
        assert!(epoch > 0);
        assert!(chunk_count > 0 && chunk_index < chunk_count);
        assert!(elections.len() <= Self::MAX_ELECTIONS);
        elections.sort_unstable();
        elections.dedup();
        let mut result = Self {
            epoch,
            reporter: reporter_key.public_key(),
            chunk_index,
            chunk_count,
            elections,
            signature: Signature::default(),
        };
        result.signature = reporter_key.sign(result.hash().as_bytes());
        result
    }

    pub fn hash(&self) -> Blake2Hash {
        let mut builder = Blake2HashBuilder::default()
            .update(b"RAI/EPOCH_REPORT_CHUNK/v1")
            .update(self.epoch.to_be_bytes())
            .update(self.reporter.as_bytes())
            .update(self.chunk_index.to_be_bytes())
            .update(self.chunk_count.to_be_bytes())
            .update((self.elections.len() as u16).to_be_bytes());
        for election in &self.elections {
            builder = builder
                .update(election.root.as_bytes())
                .update(election.previous.as_bytes());
        }
        builder.build()
    }

    pub fn validate(&self) -> bool {
        self.epoch > 0
            && self.chunk_count > 0
            && self.chunk_index < self.chunk_count
            && self.elections.len() <= Self::MAX_ELECTIONS
            && self.elections.windows(2).all(|pair| pair[0] < pair[1])
            && self
                .reporter
                .verify(self.hash().as_bytes(), &self.signature)
                .is_ok()
    }

    pub fn serialize<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.reporter.serialize(writer)?;
        writer.write_all(&self.chunk_index.to_be_bytes())?;
        writer.write_all(&self.chunk_count.to_be_bytes())?;
        writer.write_all(&(self.elections.len() as u16).to_be_bytes())?;
        for election in &self.elections {
            writer.write_all(election.root.as_bytes())?;
            writer.write_all(election.previous.as_bytes())?;
        }
        self.signature.serialize(writer)
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let mut epoch = [0; 8];
        bytes.read_exact(&mut epoch)?;
        let reporter = PublicKey::deserialize(&mut bytes)?;
        let mut two = [0; 2];
        bytes.read_exact(&mut two)?;
        let chunk_index = u16::from_be_bytes(two);
        bytes.read_exact(&mut two)?;
        let chunk_count = u16::from_be_bytes(two);
        bytes.read_exact(&mut two)?;
        let count = u16::from_be_bytes(two) as usize;
        if count > Self::MAX_ELECTIONS || bytes.len() != count * 64 + 64 {
            return Err(DeserializationError::InvalidData);
        }
        let mut elections = Vec::with_capacity(count);
        for _ in 0..count {
            elections.push(SlotRoot {
                root: Root::deserialize(&mut bytes)?,
                previous: BlockHash::deserialize(&mut bytes)?,
            });
        }
        let signature = Signature::deserialize(&mut bytes)?;
        Ok(Self {
            epoch: u64::from_be_bytes(epoch),
            reporter,
            chunk_index,
            chunk_count,
            elections,
            signature,
        })
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }
}

impl MessageVariant for EpochReportChunk {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

/// A representative's view of the finalized epoch cut at a convergence round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochFinalization {
    pub epoch: u64,
    pub round: u32,
    pub reporter: PublicKey,
    pub finalized_hash: Blake2Hash,
    pub non_cut_count: u64,
    pub signature: Signature,
}

impl EpochFinalization {
    pub const SERIALIZED_SIZE: usize = 8 + 4 + 32 + 32 + 8 + 64;

    pub fn new(
        epoch: u64,
        round: u32,
        reporter_key: &PrivateKey,
        finalized_hash: Blake2Hash,
        non_cut_count: u64,
    ) -> Self {
        let mut result = Self {
            epoch,
            round,
            reporter: reporter_key.public_key(),
            finalized_hash,
            non_cut_count,
            signature: Signature::default(),
        };
        result.signature = reporter_key.sign(result.hash().as_bytes());
        result
    }

    pub fn hash(&self) -> Blake2Hash {
        Blake2HashBuilder::default()
            .update(b"RAI/EPOCH_FINALIZATION/v1")
            .update(self.epoch.to_be_bytes())
            .update(self.round.to_be_bytes())
            .update(self.reporter.as_bytes())
            .update(self.finalized_hash.as_bytes())
            .update(self.non_cut_count.to_be_bytes())
            .build()
    }

    pub fn validate(&self) -> bool {
        self.epoch > 0
            && self
                .reporter
                .verify(self.hash().as_bytes(), &self.signature)
                .is_ok()
    }

    pub fn serialize<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&self.epoch.to_be_bytes())?;
        writer.write_all(&self.round.to_be_bytes())?;
        self.reporter.serialize(writer)?;
        writer.write_all(self.finalized_hash.as_bytes())?;
        writer.write_all(&self.non_cut_count.to_be_bytes())?;
        self.signature.serialize(writer)
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        if bytes.len() != Self::SERIALIZED_SIZE {
            return Err(DeserializationError::InvalidData);
        }
        let mut eight = [0; 8];
        bytes.read_exact(&mut eight)?;
        let epoch = u64::from_be_bytes(eight);
        let mut four = [0; 4];
        bytes.read_exact(&mut four)?;
        let round = u32::from_be_bytes(four);
        let reporter = PublicKey::deserialize(&mut bytes)?;
        let finalized_hash = Blake2Hash::deserialize(&mut bytes)?;
        bytes.read_exact(&mut eight)?;
        let non_cut_count = u64::from_be_bytes(eight);
        let signature = Signature::deserialize(&mut bytes)?;
        Ok(Self {
            epoch,
            round,
            reporter,
            finalized_hash,
            non_cut_count,
            signature,
        })
    }
}

impl MessageVariant for EpochFinalization {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn round_trip_and_validate() {
        let chunk = EpochReportChunk::new(
            2,
            &PrivateKey::from(42),
            0,
            1,
            vec![SlotRoot {
                root: Root::from(1),
                previous: BlockHash::from(2),
            }],
        );
        assert!(chunk.validate());
        assert_deserializable(&Message::EpochReportChunk(chunk));
    }

    #[test]
    fn epoch_start_round_trip() {
        assert_deserializable(&Message::EpochStart(EpochStart {
            epoch: 1,
            starts_at_unix_ms: 100,
            closes_at_unix_ms: 200,
        }));
    }

    #[test]
    fn finalization_round_trip_and_validate() {
        let report = EpochFinalization::new(1, 3, &PrivateKey::from(42), Blake2Hash::from(7), 123);
        assert!(report.validate());
        assert_deserializable(&Message::EpochFinalization(report));
    }
}
