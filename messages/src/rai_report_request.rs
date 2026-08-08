use std::io::Write;

use rsnano_types::{DeserializationError, RaiEpoch, read_u64_be};

use crate::MessageVariant;

/// Requests all validated report chunks known for an epoch. Responses use the
/// ordinary signed `RaiReport` message, so they remain independently verifiable
/// and idempotent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiReportRequest {
    pub epoch: RaiEpoch,
    /// Sender-local monotonically increasing request identifier. This keeps
    /// retries distinct from an earlier request that may have produced only a
    /// partial response.
    pub sequence: u64,
}

impl RaiReportRequest {
    pub fn serialize<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&self.epoch.number().to_be_bytes())?;
        writer.write_all(&self.sequence.to_be_bytes())
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let epoch = RaiEpoch::new(read_u64_be(&mut bytes)?);
        let sequence = read_u64_be(&mut bytes)?;
        if !bytes.is_empty() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self { epoch, sequence })
    }

    pub const fn serialized_size() -> usize {
        16
    }
}

impl MessageVariant for RaiReportRequest {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn roundtrip() {
        assert_deserializable(&Message::RaiReportRequest(RaiReportRequest {
            epoch: RaiEpoch::new(7),
            sequence: 11,
        }));
    }
}
