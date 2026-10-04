//! Application evidence envelopes, separate from fast/slow consensus payloads.
use super::*;
use rsnano_types::BlockHash;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplicationMessage {
    Candidate(CheckpointCandidate),
    Endorsement(CandidateEndorsement),
    Admission {
        candidate: CheckpointCandidate,
        admission: CandidateAdmission,
    },
    InitialRequest(InitialRRequest),
    InitialResponse(InitialRResponse),
    InitialCertificate(InitialRCertificate),
    Fetch(BlockHash),
}

/// Header: magic/version (8), checkpoint instance (32), JSON body length LE (4).
/// Decode the header before reserving body storage. Limits apply to body bytes,
/// matching SlowIoLimits::max_message_bytes.
pub struct ApplicationWireCodec {
    instance: CheckpointInstance,
    max_body_bytes: usize,
}
impl ApplicationWireCodec {
    pub const HEADER_BYTES: usize = 44;
    pub(super) const MAGIC: &'static [u8; 8] = b"RAIAPPL\x01";

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

    fn check_instance(&self, message: &ApplicationMessage) -> Result<(), CheckpointError> {
        let matches = |instance: CheckpointInstance| instance == self.instance;
        let valid = match message {
            ApplicationMessage::Candidate(c) => {
                matches(c.instance) && c.value.epoch == self.instance.epoch
            }
            ApplicationMessage::Endorsement(e) => matches(e.instance),
            ApplicationMessage::Admission {
                candidate,
                admission,
            } => {
                matches(candidate.instance)
                    && candidate.value.epoch == self.instance.epoch
                    && admission.endorsements.iter().all(|e| matches(e.instance))
            }
            ApplicationMessage::InitialRequest(r) => matches(r.instance),
            ApplicationMessage::InitialResponse(r) => matches(r.instance),
            ApplicationMessage::InitialCertificate(c) => {
                matches(c.request.instance) && c.responses.iter().all(|r| matches(r.instance))
            }
            ApplicationMessage::Fetch(_) => true,
        };
        if !valid {
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
        if &header[8..40] != self.instance.digest().as_bytes() {
            return Err(CheckpointError::WrongInstance);
        }
        let len = u32::from_le_bytes(header[40..44].try_into().unwrap()) as usize;
        if len == 0 || len > self.max_body_bytes {
            return Err(CheckpointError::InvalidSize);
        }
        Ok(len)
    }

    pub fn encode(&self, message: &ApplicationMessage) -> Result<Vec<u8>, CheckpointError> {
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
    /// The application exchange must validate candidate contents and proof signatures.
    pub fn decode(&self, bytes: &[u8]) -> Result<ApplicationMessage, CheckpointError> {
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
