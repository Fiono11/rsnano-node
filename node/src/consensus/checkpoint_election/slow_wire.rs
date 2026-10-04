//! Versioned slow-message payload codec. The node transport must frame/chunk
//! these payloads separately; this does not install a network message handler.
use super::{CheckpointError, SlowMessage};
use rsnano_types::BlockHash;

/// Header: magic/version (8), slow instance (32), JSON body length LE (4).
/// Decode the header before reserving body storage. Limits apply to body bytes,
/// matching SlowIoLimits::max_message_bytes.
pub struct SlowWireCodec {
    instance: BlockHash,
    max_body_bytes: usize,
}
impl SlowWireCodec {
    pub const HEADER_BYTES: usize = 44;
    pub(super) const MAGIC: &'static [u8; 8] = b"RAISLOW\x01";

    pub fn new(instance: BlockHash, max_body_bytes: usize) -> Result<Self, CheckpointError> {
        if max_body_bytes == 0 || max_body_bytes > u32::MAX as usize {
            return Err(CheckpointError::InvalidSize);
        }
        Ok(Self {
            instance,
            max_body_bytes,
        })
    }

    fn check_instance(&self, message: &SlowMessage) -> Result<(), CheckpointError> {
        let instance = match message {
            SlowMessage::Request(r) => r.instance,
            SlowMessage::Response(r) => r.instance,
            SlowMessage::Certificate(c) => c.instance,
            // Fetch and decision hints are scoped by the envelope, and their
            // referenced evidence still needs recursive verification.
            SlowMessage::Fetch(_) | SlowMessage::Decision(_) => self.instance,
        };
        if instance != self.instance {
            return Err(CheckpointError::WrongInstance);
        }
        Ok(())
    }

    /// Returns the body size only for an exact, supported header in this instance.
    /// Transport callers must use this check before allocating a body buffer.
    pub fn body_len(&self, header: &[u8]) -> Result<usize, CheckpointError> {
        if header.len() != Self::HEADER_BYTES {
            return Err(CheckpointError::InvalidSize);
        }
        if &header[..8] != Self::MAGIC {
            return Err(CheckpointError::InvalidEvidence);
        }
        if &header[8..40] != self.instance.as_bytes() {
            return Err(CheckpointError::WrongInstance);
        }
        let len = u32::from_le_bytes(header[40..44].try_into().unwrap()) as usize;
        if len == 0 || len > self.max_body_bytes {
            return Err(CheckpointError::InvalidSize);
        }
        Ok(len)
    }

    pub fn encode(&self, message: &SlowMessage) -> Result<Vec<u8>, CheckpointError> {
        self.check_instance(message)?;
        let len = message.encoded_len(self.max_body_bytes)?;
        let mut bytes = Vec::with_capacity(Self::HEADER_BYTES + len);
        bytes.extend_from_slice(Self::MAGIC);
        bytes.extend_from_slice(self.instance.as_bytes());
        bytes.extend_from_slice(&(len as u32).to_le_bytes());
        serde_json::to_writer(&mut bytes, message).map_err(|_| CheckpointError::InvalidEvidence)?;
        Ok(bytes)
    }

    /// Structural decoding is not signature, membership or proof validation.
    /// Pass the result through SlowReplica::receive before acting on it.
    pub fn decode(&self, bytes: &[u8]) -> Result<SlowMessage, CheckpointError> {
        let header = bytes
            .get(..Self::HEADER_BYTES)
            .ok_or(CheckpointError::InvalidSize)?;
        let len = self.body_len(header)?;
        if bytes.len() - Self::HEADER_BYTES != len {
            return Err(CheckpointError::InvalidSize);
        }
        let message = serde_json::from_slice(&bytes[Self::HEADER_BYTES..])
            .map_err(|_| CheckpointError::InvalidEvidence)?;
        self.check_instance(&message)?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_bounds_and_malformed_envelopes() {
        let message = SlowMessage::Fetch(BlockHash::from(7));
        let len = message.encoded_len(1024).unwrap();
        let codec = SlowWireCodec::new(BlockHash::from(1), len).unwrap();
        let bytes = codec.encode(&message).unwrap();
        assert_eq!(codec.decode(&bytes).unwrap(), message);
        for end in 0..bytes.len() {
            assert!(codec.decode(&bytes[..end]).is_err());
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(codec.decode(&extra), Err(CheckpointError::InvalidSize));
        let small = SlowWireCodec::new(BlockHash::from(1), len - 1).unwrap();
        assert!(small.encode(&message).is_err());
        assert!(small.body_len(&bytes[..44]).is_err());
        let other = SlowWireCodec::new(BlockHash::from(2), len).unwrap();
        assert_eq!(other.decode(&bytes), Err(CheckpointError::WrongInstance));
        let mut version = bytes.clone();
        version[7] = 2;
        assert_eq!(
            codec.decode(&version),
            Err(CheckpointError::InvalidEvidence)
        );
        let mut length = bytes.clone();
        length[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(codec.body_len(&length[..44]).is_err());
        let mut invalid = bytes;
        invalid[44] = 0xff;
        assert_eq!(
            codec.decode(&invalid),
            Err(CheckpointError::InvalidEvidence)
        );
    }
}
