use bitvec::prelude::BitArray;

use rsnano_types::{
    Blake2HashBuilder, BlockHash, ConsensusEpoch, DeserializationError, PrivateKey, PublicKey,
    Signature,
};

use crate::MessageVariant;

/// RAI, "Retained evidence": a member's signed acknowledgement that it
/// installed the decided checkpoint of an epoch. Once members of the
/// successor committee carrying `N - f` of its weight have acknowledged a
/// checkpoint, the handoff evidence that produced it, the epoch's frozen
/// reports and their reconstructions, may be released: "Departing validators
/// continue service until correct successors hold durable copies."
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochInstalled {
    pub epoch: ConsensusEpoch,
    /// d_e: the state hash of the checkpoint installed
    pub state: BlockHash,
    pub member: PublicKey,
    pub signature: Signature,
}

impl EpochInstalled {
    pub const SERIALIZED_SIZE: usize = ConsensusEpoch::SERIALIZED_SIZE
        + BlockHash::SERIALIZED_SIZE
        + PublicKey::SERIALIZED_SIZE
        + Signature::SERIALIZED_SIZE;

    pub fn new(key: &PrivateKey, epoch: ConsensusEpoch, state: BlockHash) -> Self {
        let member = key.public_key();
        let payload = Self::payload(epoch, &state, &member);
        Self {
            epoch,
            state,
            member,
            signature: key.sign(payload.as_bytes()),
        }
    }

    pub fn new_test_instance() -> Self {
        Self::new(
            &PrivateKey::from(5),
            ConsensusEpoch::new(2),
            BlockHash::from(3),
        )
    }

    fn payload(epoch: ConsensusEpoch, state: &BlockHash, member: &PublicKey) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI epoch installed")
            .update(epoch.as_u64().to_le_bytes())
            .update(state.as_bytes())
            .update(member.as_bytes())
            .build()
    }

    pub fn verify(&self) -> bool {
        let payload = Self::payload(self.epoch, &self.state, &self.member);
        self.member
            .verify(payload.as_bytes(), &self.signature)
            .is_ok()
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        self.state.serialize(writer)?;
        self.member.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub const fn serialized_size(_extensions: BitArray<u16>) -> usize {
        Self::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let message = Self {
            epoch: ConsensusEpoch::deserialize(bytes)?,
            state: BlockHash::deserialize(bytes)?,
            member: PublicKey::deserialize(bytes)?,
            signature: Signature::deserialize(bytes)?,
        };
        if !bytes.is_empty() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(message)
    }
}

impl MessageVariant for EpochInstalled {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        BitArray::ZERO
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn serialize_an_epoch_installed_acknowledgement() {
        let message = EpochInstalled::new_test_instance();
        assert!(message.verify());
        assert_deserializable(&Message::EpochInstalled(message));
    }

    #[test]
    fn a_tampered_acknowledgement_does_not_verify() {
        let mut message = EpochInstalled::new_test_instance();
        message.state = BlockHash::from(4);
        assert!(!message.verify());
    }
}
