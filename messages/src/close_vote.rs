use bitvec::array::BitArray;
use rsnano_types::{
    Blake2Hash, Blake2HashBuilder, BlockHash, DeserializationError, PrivateKey, PublicKey,
    Signature, VoteType, read_u8, read_u32_be, read_u64_be,
};

use crate::MessageVariant;

const DOMAIN: &[u8] = b"RAI/CloseVote";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseVote {
    pub epoch: u64,
    pub round: u32,
    pub kind: u8,
    pub value: BlockHash,
    pub vote_type: VoteType,
    pub voter: PublicKey,
    pub signature: Signature,
}

impl CloseVote {
    /// A signed record-hash advertisement used to synchronize record election
    /// startup. It deliberately shares the close-vote envelope, but is routed
    /// outside the election tally.
    pub const RECORD_HASH_ADVERTISEMENT_KIND: u8 = 2;
    pub const SERIALIZED_SIZE: usize = 8 + 4 + 1 + 32 + 1 + 32 + 64;

    pub fn new(
        epoch: u64,
        round: u32,
        kind: u8,
        value: BlockHash,
        vote_type: VoteType,
        key: &PrivateKey,
    ) -> Self {
        let voter = key.public_key();
        let hash = vote_hash(epoch, round, kind, value, vote_type, voter);
        Self {
            epoch,
            round,
            kind,
            value,
            vote_type,
            voter,
            signature: key.sign(hash.as_bytes()),
        }
    }

    pub fn hash(&self) -> Blake2Hash {
        vote_hash(
            self.epoch,
            self.round,
            self.kind,
            self.value,
            self.vote_type,
            self.voter,
        )
    }

    pub fn validate(&self) -> bool {
        (self.kind <= 1
            || (self.kind == Self::RECORD_HASH_ADVERTISEMENT_KIND
                && self.vote_type == VoteType::First))
            && self
                .voter
                .verify(self.hash().as_bytes(), &self.signature)
                .is_ok()
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&self.epoch.to_be_bytes())?;
        writer.write_all(&self.round.to_be_bytes())?;
        writer.write_all(&[self.kind])?;
        self.value.serialize(writer)?;
        writer.write_all(&[self.vote_type as u8])?;
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        if bytes.len() != Self::SERIALIZED_SIZE {
            return Err(DeserializationError::InvalidData);
        }
        let epoch = read_u64_be(&mut bytes)?;
        let round = read_u32_be(&mut bytes)?;
        let kind = read_u8(&mut bytes)?;
        let value = BlockHash::deserialize(&mut bytes)?;
        let vote_type = match read_u8(&mut bytes)? {
            0 => VoteType::NonFinal,
            1 => VoteType::Final,
            2 => VoteType::First,
            3 => VoteType::Timeout,
            _ => return Err(DeserializationError::InvalidData),
        };
        let voter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        Ok(Self {
            epoch,
            round,
            kind,
            value,
            vote_type,
            voter,
            signature,
        })
    }

    pub const fn serialized_size(_: BitArray<u16>) -> usize {
        Self::SERIALIZED_SIZE
    }
}

impl MessageVariant for CloseVote {}

fn vote_hash(
    epoch: u64,
    round: u32,
    kind: u8,
    value: BlockHash,
    vote_type: VoteType,
    voter: PublicKey,
) -> Blake2Hash {
    Blake2HashBuilder::default()
        .update(DOMAIN)
        .update(epoch.to_be_bytes())
        .update(round.to_be_bytes())
        .update([kind])
        .update(value.as_bytes())
        .update([vote_type as u8])
        .update(voter.as_bytes())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn roundtrip_and_signature() {
        let vote = CloseVote::new(
            2,
            3,
            0,
            BlockHash::from(4),
            VoteType::First,
            &PrivateKey::from(1),
        );
        assert!(vote.validate());
        assert_deserializable(&Message::CloseVote(vote));
    }

    #[test]
    fn record_hash_advertisement_roundtrips_and_validates() {
        let advertisement = CloseVote::new(
            7,
            2,
            CloseVote::RECORD_HASH_ADVERTISEMENT_KIND,
            BlockHash::from(9),
            VoteType::First,
            &PrivateKey::from(1),
        );

        assert!(advertisement.validate());
        assert_deserializable(&Message::CloseVote(advertisement));
    }
}
