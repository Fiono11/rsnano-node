use crate::MessageVariant;
use bitvec::prelude::BitArray;
use rsnano_types::{
    Blake2HashBuilder, BlockHash, DeserializationError, PrivateKey, PublicKey, Signature,
};
use serde::{Deserialize, Serialize};

/// A close-round statement, or a bounded page of its immutable snapshot.
/// Vote kinds 0..=4 have the same meaning as VoteKind; 5 transports snapshot data; 6 acknowledges a persisted close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochClose {
    pub epoch: u64,
    pub round: u64,
    pub parent: BlockHash,
    pub state: BlockHash,
    pub kind: u8,
    pub voter: PublicKey,
    pub signature: Signature,
    pub page: u16,
    pub pages: u16,
    pub hashes: Vec<BlockHash>,
}
impl EpochClose {
    pub const PAGE_SIZE: usize = 512;
    pub const MAX_PAGES: u16 = 2048;
    pub fn candidate_id(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"rai-close-candidate-v1")
            .update(self.epoch.to_le_bytes())
            .update(self.round.to_le_bytes())
            .update(self.parent.as_bytes())
            .update(self.state.as_bytes())
            .build()
    }
    pub fn signing_hash(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"rai-close-vote-v1")
            .update(self.candidate_id().as_bytes())
            .update([self.kind])
            .build()
    }
    pub fn sign(&mut self, key: &PrivateKey) {
        self.voter = key.public_key();
        self.signature = key.sign(self.signing_hash().as_bytes());
    }
    pub fn valid_vote(&self) -> bool {
        self.kind <= 4
            && self.hashes.is_empty()
            && self.pages == 0
            && self.page == 0
            && (!matches!(self.kind, 3 | 4) || (self.parent.is_zero() && self.state.is_zero()))
            && self
                .voter
                .verify(self.signing_hash().as_bytes(), &self.signature)
                .is_ok()
    }
    /// Receipt only: never usable as a consensus vote or certificate.
    pub fn valid_receipt(&self) -> bool {
        self.kind == 6
            && self.round == 0
            && self.parent.is_zero()
            && self.hashes.is_empty()
            && self.pages == 0
            && self.page == 0
            && self
                .voter
                .verify(self.signing_hash().as_bytes(), &self.signature)
                .is_ok()
    }

    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        serde_json::to_writer(writer, self).map_err(std::io::Error::other)
    }
    pub fn deserialize(payload: &[u8]) -> Result<Self, DeserializationError> {
        let value: Self =
            serde_json::from_slice(payload).map_err(|_| DeserializationError::InvalidData)?;
        if value.kind > 6 || value.hashes.len() > Self::PAGE_SIZE || value.pages > Self::MAX_PAGES {
            return Err(DeserializationError::InvalidData);
        }
        Ok(value)
    }
}
impl MessageVariant for EpochClose {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_receipt_cannot_be_used_as_a_vote() {
        let mut receipt = EpochClose {
            epoch: 1,
            round: 0,
            parent: BlockHash::ZERO,
            state: 9.into(),
            kind: 6,
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            page: 0,
            pages: 0,
            hashes: vec![],
        };
        receipt.sign(&PrivateKey::from(1));
        assert!(receipt.valid_receipt());
        assert!(!receipt.valid_vote());
        crate::assert_deserializable(&crate::Message::EpochClose(receipt.clone()));
        receipt.state = 10.into();
        assert!(!receipt.valid_receipt());
    }

    #[test]
    fn close_signature_binds_epoch_round_parent_state_and_kind() {
        let mut v = EpochClose {
            epoch: 3,
            round: 7,
            parent: 8.into(),
            state: 9.into(),
            kind: 0,
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            page: 0,
            pages: 0,
            hashes: vec![],
        };
        v.sign(&PrivateKey::from(1));
        assert!(v.valid_vote());
        for field in 0..5 {
            let mut changed = v.clone();
            match field {
                0 => changed.epoch += 1,
                1 => changed.round += 1,
                2 => changed.parent = 10.into(),
                3 => changed.state = 10.into(),
                _ => changed.kind = 1,
            }
            assert!(!changed.valid_vote());
        }
        let message = crate::Message::EpochClose(v);
        crate::assert_deserializable(&message);
    }
}
