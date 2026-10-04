use crate::MessageVariant;
use bitvec::prelude::BitArray;
use rsnano_types::DeserializationError;

/// Structurally checked v1 checkpoint chunk (fast or slow payload). Authentication and content-digest checks
/// belong to the checkpoint engine after per-connection reassembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointFrame(Vec<u8>);
impl CheckpointFrame {
    pub const MAX_BYTES: usize = 65535 - 8;
    pub const HEADER_BYTES: usize = 48;
    pub const CHUNK_BYTES: usize = Self::MAX_BYTES - Self::HEADER_BYTES;
    pub const MAGIC: &'static [u8; 8] = b"RAICHNK\x01";

    pub fn validate(bytes: &[u8]) -> Result<(), DeserializationError> {
        if bytes.len() <= Self::HEADER_BYTES || bytes.len() > Self::MAX_BYTES {
            return Err(DeserializationError::InvalidData);
        }
        if &bytes[..8] != Self::MAGIC {
            return Err(DeserializationError::InvalidData);
        }
        let total = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(bytes[44..48].try_into().unwrap()) as usize;
        if offset >= total
            || offset % Self::CHUNK_BYTES != 0
            || bytes.len() - Self::HEADER_BYTES != Self::CHUNK_BYTES.min(total - offset)
        {
            return Err(DeserializationError::InvalidData);
        }
        Ok(())
    }
    pub fn new(bytes: Vec<u8>) -> Result<Self, DeserializationError> {
        Self::validate(&bytes)?;
        Ok(Self(bytes))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn deserialize(
        bytes: &[u8],
        extensions: BitArray<u16>,
    ) -> Result<Self, DeserializationError> {
        if bytes.len() != extensions.data as usize {
            return Err(DeserializationError::InvalidData);
        }
        Self::validate(bytes)?;
        Ok(Self(bytes.to_vec()))
    }
    pub fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(&self.0)
    }
}
impl MessageVariant for CheckpointFrame {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        (self.0.len() as u16).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, MessageDeserializer, MessageSerializer, MessageType, ParseMessageError};
    fn frame(len: usize) -> CheckpointFrame {
        let mut bytes = vec![0; CheckpointFrame::HEADER_BYTES + len];
        bytes[..8].copy_from_slice(CheckpointFrame::MAGIC);
        bytes[40..44].copy_from_slice(&(len as u32).to_le_bytes());
        CheckpointFrame::new(bytes).unwrap()
    }
    #[test]
    fn stream_roundtrip_with_partial_reads_and_consecutive_frames() {
        let message = Message::CheckpointFrame(frame(CheckpointFrame::CHUNK_BYTES));
        let mut serializer = MessageSerializer::default();
        let bytes = serializer.serialize(&message).to_vec();
        assert_eq!(bytes.len(), 65535);
        assert_eq!(bytes[5], 0x1a);
        crate::assert_deserializable(&message);
        let mut decoder = MessageDeserializer::new(Default::default());
        for _ in 0..2 {
            for chunk in bytes[..bytes.len() - 1].chunks(997) {
                decoder.push(chunk);
                assert!(decoder.try_deserialize().is_none());
            }
            decoder.push(&bytes[bytes.len() - 1..]);
            assert_eq!(decoder.try_deserialize().unwrap().unwrap().message, message);
        }
    }
    #[test]
    fn rejects_bad_frame_and_oversized_header() {
        let message = Message::CheckpointFrame(frame(1));
        let mut serializer = MessageSerializer::default();
        let mut bytes = serializer.serialize(&message).to_vec();
        bytes[15] = 2;
        let mut decoder = MessageDeserializer::new(Default::default());
        decoder.push(&bytes);
        assert_eq!(
            decoder.try_deserialize().unwrap(),
            Err(ParseMessageError::InvalidMessage(
                MessageType::CheckpointFrame
            ))
        );
        bytes[6..8].copy_from_slice(&65528u16.to_le_bytes());
        let mut decoder = MessageDeserializer::new(Default::default());
        decoder.push(&bytes[..8]);
        assert_eq!(
            decoder.try_deserialize().unwrap(),
            Err(ParseMessageError::MessageSizeTooBig)
        );
        assert!(CheckpointFrame::new(vec![0; 48]).is_err());
        assert!(CheckpointFrame::deserialize(frame(1).as_bytes(), 50u16.into()).is_err());
    }
}
