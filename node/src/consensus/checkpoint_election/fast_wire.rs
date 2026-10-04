//! Versioned fast-message payload codec. The node transport must frame/chunk
//! these payloads separately; this does not install a network message handler.
use super::{CheckpointError, CheckpointInstance, FastCertificate, FirstVote, RecoveryCertificate};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FastMessage {
    First(FirstVote),
    Certificate(FastCertificate),
    Recovery(RecoveryCertificate),
}

/// Header: magic/version (8), fast instance (32), JSON body length LE (4).
/// Decode the header before reserving body storage. Limits apply to body bytes,
/// matching SlowIoLimits::max_message_bytes.
pub struct FastWireCodec {
    instance: CheckpointInstance,
    max_body_bytes: usize,
}
impl FastWireCodec {
    pub const HEADER_BYTES: usize = 44;
    pub(super) const MAGIC: &'static [u8; 8] = b"RAIFAST\x01";

    pub fn new(
        instance: CheckpointInstance,
        max_body_bytes: usize,
    ) -> Result<Self, CheckpointError> {
        if max_body_bytes == 0 || max_body_bytes > u32::MAX as usize {
            return Err(CheckpointError::InvalidSize);
        }
        Ok(Self {
            instance,
            max_body_bytes,
        })
    }

    fn check_instance(&self, message: &FastMessage) -> Result<(), CheckpointError> {
        let (instance, rank, votes): (_, _, &[FirstVote]) = match message {
            FastMessage::First(v) => (v.instance, v.rank, std::slice::from_ref(v)),
            FastMessage::Certificate(c) => (c.instance, c.rank, &c.votes),
            FastMessage::Recovery(c) => (c.instance, c.rank, &c.snapshot),
        };
        if instance != self.instance || votes.iter().any(|v| v.instance != self.instance) {
            return Err(CheckpointError::WrongInstance);
        }
        if rank != 0 || votes.iter().any(|v| v.rank != 0) {
            return Err(CheckpointError::WrongRank);
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
        if &header[8..40] != self.instance.digest().as_bytes() {
            return Err(CheckpointError::WrongInstance);
        }
        let len = u32::from_le_bytes(header[40..44].try_into().unwrap()) as usize;
        if len == 0 || len > self.max_body_bytes {
            return Err(CheckpointError::InvalidSize);
        }
        Ok(len)
    }

    pub fn encode(&self, message: &FastMessage) -> Result<Vec<u8>, CheckpointError> {
        self.check_instance(message)?;
        let len = super::slow_io::encoded_len(message, self.max_body_bytes)?;
        let mut bytes = Vec::with_capacity(Self::HEADER_BYTES + len);
        bytes.extend_from_slice(Self::MAGIC);
        bytes.extend_from_slice(self.instance.digest().as_bytes());
        bytes.extend_from_slice(&(len as u32).to_le_bytes());
        serde_json::to_writer(&mut bytes, message).map_err(|_| CheckpointError::InvalidEvidence)?;
        Ok(bytes)
    }

    /// Structural decoding is not signature, membership or proof validation.
    /// Verify FIRST with CheckpointContext and certificates with their verify methods
    /// before counting votes, admitting fallback or installing decisions.
    pub fn decode(&self, bytes: &[u8]) -> Result<FastMessage, CheckpointError> {
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
