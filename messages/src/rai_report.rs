use std::io::Write;

use bitvec::prelude::BitArray;
use rsnano_types::{
    DeserializationError, PublicKey, QualifiedRoot, RaiEpoch, RaiSlotId, Signature, read_u64_be,
};

use crate::MessageVariant;

/// Wire representation of an epoch visibility report. Signature validation is
/// deliberately performed by the consensus report store after conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiReportMessage {
    pub reporter: PublicKey,
    pub epoch: RaiEpoch,
    pub visible_obligations: Vec<RaiSlotId>,
    pub signature: Signature,
}

impl RaiReportMessage {
    pub fn serialize<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.reporter.serialize(writer)?;
        writer.write_all(&self.epoch.number().to_be_bytes())?;
        self.signature.serialize(writer)?;
        for slot in &self.visible_obligations {
            writer.write_all(&slot.epoch.number().to_be_bytes())?;
            slot.root.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let reporter = PublicKey::deserialize(&mut bytes)?;
        let epoch = RaiEpoch::new(read_u64_be(&mut bytes)?);
        let signature = Signature::deserialize(&mut bytes)?;
        const SLOT_SIZE: usize = 8 + QualifiedRoot::SERIALIZED_SIZE;
        if bytes.len() % SLOT_SIZE != 0 {
            return Err(DeserializationError::InvalidData);
        }
        let mut visible_obligations = Vec::new();
        while !bytes.is_empty() {
            visible_obligations.push(RaiSlotId {
                epoch: RaiEpoch::new(read_u64_be(&mut bytes)?),
                root: QualifiedRoot::deserialize(&mut bytes)?,
            });
        }
        visible_obligations.sort();
        visible_obligations.dedup();
        Ok(Self {
            reporter,
            epoch,
            visible_obligations,
            signature,
        })
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }
}

impl MessageVariant for RaiReportMessage {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn roundtrip() {
        assert_deserializable(&Message::RaiReport(RaiReportMessage {
            reporter: PublicKey::from(1),
            epoch: RaiEpoch::new(2),
            visible_obligations: vec![RaiSlotId {
                epoch: RaiEpoch::new(2),
                root: QualifiedRoot::new_test_instance(),
            }],
            signature: Signature::from_bytes([3; 64]),
        }));
    }
}
