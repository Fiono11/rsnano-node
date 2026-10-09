use bitvec::prelude::BitArray;

use rsnano_types::{BlockHash, ConsensusEpoch, DeserializationError};

use crate::MessageVariant;

/// RAI, checkpoint catch-up: a request for the decided state `S_e` of an
/// epoch, by its hash `d_e`, from byte `from` of its encoding on. A replica
/// that learned the close certificate of an epoch but could not derive the
/// value - the reports it needed were released while it lagged behind -
/// fetches the state the certified value names and checks it against `d_e`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointReq {
    pub epoch: ConsensusEpoch,
    pub state: BlockHash,
    pub from: u32,
}

impl CheckpointReq {
    pub const SERIALIZED_SIZE: usize =
        ConsensusEpoch::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 4;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(3),
            state: BlockHash::from(7),
            from: 60_000,
        }
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        self.state.serialize(writer)?;
        writer.write_all(&self.from.to_le_bytes())
    }

    pub const fn serialized_size(_extensions: BitArray<u16>) -> usize {
        Self::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let state = BlockHash::deserialize(bytes)?;
        let from = u32::from_le_bytes(take::<4>(bytes)?);
        if !bytes.is_empty() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self { epoch, state, from })
    }
}

impl MessageVariant for CheckpointReq {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        BitArray::ZERO
    }
}

/// RAI, checkpoint catch-up: one chunk of the encoding of `S_e`, the bytes
/// `from .. from + data.len()` of `total`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointReply {
    pub epoch: ConsensusEpoch,
    pub state: BlockHash,
    pub total: u32,
    pub from: u32,
    pub data: Vec<u8>,
}

impl CheckpointReply {
    /// A full chunk's payload length must fit the 16-bit header extension
    /// and the message size limit
    pub const MAX_DATA: usize = 60_000;
    const HEAD: usize = ConsensusEpoch::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 8;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(3),
            state: BlockHash::from(7),
            total: 70_000,
            from: 60_000,
            data: vec![1, 2, 3],
        }
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        self.state.serialize(writer)?;
        writer.write_all(&self.total.to_le_bytes())?;
        writer.write_all(&self.from.to_le_bytes())?;
        writer.write_all(&self.data)
    }

    /// The payload length is carried in the extensions
    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let state = BlockHash::deserialize(bytes)?;
        let total = u32::from_le_bytes(take::<4>(bytes)?);
        let from = u32::from_le_bytes(take::<4>(bytes)?);
        if bytes.len() > Self::MAX_DATA {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            epoch,
            state,
            total,
            from,
            data: bytes.to_vec(),
        })
    }
}

impl MessageVariant for CheckpointReply {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        BitArray::new((Self::HEAD + self.data.len().min(Self::MAX_DATA)) as u16)
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
    fn serialize_a_checkpoint_request() {
        assert_deserializable(&Message::CheckpointReq(CheckpointReq::new_test_instance()));
    }

    #[test]
    fn serialize_a_checkpoint_reply() {
        assert_deserializable(&Message::CheckpointReply(
            CheckpointReply::new_test_instance(),
        ));
    }

    /// A full chunk's length fits the header extension it is carried in
    #[test]
    fn a_full_checkpoint_reply_fits_its_header() {
        let reply = CheckpointReply {
            data: vec![9; CheckpointReply::MAX_DATA],
            ..CheckpointReply::new_test_instance()
        };
        let length = CheckpointReply::HEAD + CheckpointReply::MAX_DATA;
        assert!(length <= u16::MAX as usize);
        assert!(length <= crate::Message::MAX_MESSAGE_SIZE);
        assert_deserializable(&Message::CheckpointReply(reply));
    }
}
