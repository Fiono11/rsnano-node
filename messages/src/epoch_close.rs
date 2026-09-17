use crate::MessageVariant;
use bitvec::prelude::BitArray;
use rsnano_types::{
    Blake2HashBuilder, BlockHash, DeserializationError, PrivateKey, PublicKey, Signature,
};
use serde::{Deserialize, Serialize};

/// A digest-only close statement. Vote kinds 0..=4 carry only a digest and 6
/// acknowledges a persisted close. Kind 5 is the close assembler's proposal of
/// rule C2: its own membership root paired with a ParentOK parent. Memberships
/// converge through the elections themselves, so no close packet names members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochClose {
    pub epoch: u64,
    /// Close round.
    pub round: u64,
    pub parent: BlockHash,
    /// Identity of the previous finalized epoch close, separate from the round parent.
    pub previous_close: BlockHash,
    /// Membership tree root: the proposed close.
    pub state: BlockHash,
    pub kind: u8,
    pub voter: PublicKey,
    pub signature: Signature,
    /// Member count of the proposer's membership.
    #[serde(default)]
    pub members: u64,
}
impl EpochClose {
    pub const PROPOSAL: u8 = 5;
    pub const RECEIPT: u8 = 6;
    pub fn candidate_id(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"rai-close-candidate-v2")
            .update(self.epoch.to_le_bytes())
            .update(self.round.to_le_bytes())
            .update(self.parent.as_bytes())
            .update(self.previous_close.as_bytes())
            .update(self.state.as_bytes())
            .build()
    }
    pub fn signing_hash(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"rai-close-vote-v2")
            .update(self.candidate_id().as_bytes())
            .update([self.kind])
            .update(self.members.to_le_bytes())
            .build()
    }
    pub fn sign(&mut self, key: &PrivateKey) {
        self.voter = key.public_key();
        self.signature = key.sign(self.signing_hash().as_bytes());
    }
    fn signature_valid(&self) -> bool {
        self.voter
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .is_ok()
    }
    pub fn valid_vote(&self) -> bool {
        self.kind <= 4
            && (!matches!(self.kind, 3 | 4) || (self.parent.is_zero() && self.state.is_zero()))
            && self.signature_valid()
    }
    /// Receipt only: never usable as a consensus vote or certificate.
    pub fn valid_receipt(&self) -> bool {
        self.kind == Self::RECEIPT
            && self.round == 0
            && self.parent.is_zero()
            && self.signature_valid()
    }
    /// Close assembler proposal (rule C2): the round assembler's signature on
    /// the exact round-scoped value it publishes, its membership root paired
    /// with the parent it selected. Never a consensus vote.
    pub fn valid_proposal(&self) -> bool {
        self.kind == Self::PROPOSAL && !self.state.is_zero() && self.signature_valid()
    }

    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        serde_json::to_writer(writer, self).map_err(std::io::Error::other)
    }
    pub fn deserialize(payload: &[u8]) -> Result<Self, DeserializationError> {
        let value: Self =
            serde_json::from_slice(payload).map_err(|_| DeserializationError::InvalidData)?;
        if value.kind > Self::RECEIPT {
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

    fn packet(kind: u8) -> EpochClose {
        EpochClose {
            epoch: 0,
            round: 0,
            previous_close: BlockHash::ZERO,
            parent: BlockHash::ZERO,
            state: 3.into(),
            kind,
            voter: 0.into(),
            signature: Signature::new(),
            members: 0,
        }
    }

    #[test]
    fn proposal_binds_the_value_and_never_votes() {
        let mut proposal = packet(EpochClose::PROPOSAL);
        proposal.round = 2;
        proposal.parent = 5.into();
        proposal.members = 9;
        proposal.sign(&PrivateKey::from(6));
        assert!(proposal.valid_proposal());
        assert!(!proposal.valid_vote());
        assert!(!proposal.valid_receipt());
        crate::assert_deserializable(&crate::Message::EpochClose(proposal.clone()));
        for (field, change) in [
            (
                "round",
                Box::new(|p: &mut EpochClose| p.round = 3) as Box<dyn Fn(&mut EpochClose)>,
            ),
            ("parent", Box::new(|p: &mut EpochClose| p.parent = 6.into())),
            ("state", Box::new(|p: &mut EpochClose| p.state = 4.into())),
            (
                "previous close",
                Box::new(|p: &mut EpochClose| p.previous_close = 1.into()),
            ),
            ("members", Box::new(|p: &mut EpochClose| p.members = 10)),
        ] {
            let mut changed = proposal.clone();
            change(&mut changed);
            assert!(!changed.valid_proposal(), "the {field} is signed");
        }
        let mut empty = proposal.clone();
        empty.state = BlockHash::ZERO;
        empty.sign(&PrivateKey::from(6));
        assert!(!empty.valid_proposal(), "a proposal names a root");
    }

    #[test]
    fn unknown_kinds_are_rejected_on_the_wire() {
        let mut packet = packet(7);
        packet.sign(&PrivateKey::from(1));
        let mut bytes = Vec::new();
        packet.serialize(&mut bytes).unwrap();
        assert!(EpochClose::deserialize(&bytes).is_err());
    }

    #[test]
    fn close_receipt_cannot_be_used_as_a_vote() {
        let mut receipt = packet(EpochClose::RECEIPT);
        receipt.epoch = 1;
        receipt.state = 9.into();
        receipt.sign(&PrivateKey::from(1));
        assert!(receipt.valid_receipt());
        assert!(!receipt.valid_vote());
        assert!(!receipt.valid_proposal());
        crate::assert_deserializable(&crate::Message::EpochClose(receipt.clone()));
        receipt.state = 10.into();
        assert!(!receipt.valid_receipt());
    }

    #[test]
    fn close_signature_binds_epoch_round_parent_state_and_kind() {
        let mut v = packet(0);
        v.epoch = 3;
        v.round = 7;
        v.parent = 8.into();
        v.state = 9.into();
        v.sign(&PrivateKey::from(1));
        assert!(v.valid_vote());
        for field in 0..6 {
            let mut changed = v.clone();
            match field {
                0 => changed.epoch += 1,
                1 => changed.round += 1,
                2 => changed.parent = 10.into(),
                3 => changed.state = 10.into(),
                4 => changed.members += 1,
                _ => changed.kind = 1,
            }
            assert!(!changed.valid_vote());
        }
        let mut timeout = packet(3);
        timeout.state = BlockHash::ZERO;
        timeout.sign(&PrivateKey::from(1));
        assert!(timeout.valid_vote());
        timeout.state = 1.into();
        timeout.sign(&PrivateKey::from(1));
        assert!(!timeout.valid_vote(), "a timeout names no value");
        let message = crate::Message::EpochClose(v);
        crate::assert_deserializable(&message);
    }
}
