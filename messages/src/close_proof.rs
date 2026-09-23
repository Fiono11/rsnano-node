use crate::{EpochProp, MessageVariant};
use bitvec::prelude::BitArray;
use rsnano_types::{ConsensusEpoch, DeserializationError, Vote};

/// Request the certificate of a decided epoch, without reopening its election.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseProofReq {
    pub epoch: ConsensusEpoch,
}

/// Original signed placement and individually signed votes. The receiver must
/// verify these with its own predecessor-derived committees before trusting d_e.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseProofReply {
    pub proposal: EpochProp,
    pub votes: Vec<Vote>,
}

impl CloseProofReq {
    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        self.epoch.serialize(writer)
    }
    pub const fn serialized_size(_: BitArray<u16>) -> usize {
        8
    }
    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let epoch = ConsensusEpoch::deserialize(&mut bytes)?;
        if !bytes.is_empty() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self { epoch })
    }
}
impl MessageVariant for CloseProofReq {}

impl CloseProofReply {
    pub const MAX_VOTES: usize = 128;
    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        let mut body = Vec::new();
        let mut proposal = Vec::new();
        self.proposal.serialize(&mut proposal)?;
        body.extend_from_slice(&(proposal.len() as u16).to_le_bytes());
        body.extend(proposal);
        for vote in &self.votes {
            body.extend_from_slice(
                &(Vote::serialized_size(vote.hashes.len()) as u16).to_le_bytes(),
            );
            vote.serialize(&mut body)?;
        }
        if self.votes.len() > Self::MAX_VOTES || body.len() > u16::MAX as usize {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        writer.write_all(&body)
    }
    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }
    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let proposal = EpochProp::deserialize(take_object(&mut bytes)?)?;
        let mut votes = Vec::new();
        while !bytes.is_empty() {
            if votes.len() == Self::MAX_VOTES {
                return Err(DeserializationError::InvalidData);
            }
            let encoded = take_object(&mut bytes)?;
            let vote = Vote::deserialize(encoded)?;
            if encoded.len() != Vote::serialized_size(vote.hashes.len()) {
                return Err(DeserializationError::InvalidData);
            }
            if vote.hashes.is_empty() || vote.hashes.len() > Vote::MAX_HASHES {
                return Err(DeserializationError::InvalidData);
            }
            votes.push(vote);
        }
        Ok(Self { proposal, votes })
    }
}
impl MessageVariant for CloseProofReply {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

fn take_object<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8], DeserializationError> {
    if bytes.len() < 2 {
        return Err(DeserializationError::InvalidData);
    }
    let len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    *bytes = &bytes[2..];
    if bytes.len() < len {
        return Err(DeserializationError::InvalidData);
    }
    let (object, rest) = bytes.split_at(len);
    *bytes = rest;
    Ok(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};
    use rsnano_types::{BlockHash, PrivateKey, UnixMillisTimestamp, VoteKind};

    #[test]
    fn proof_messages_roundtrip() {
        assert_deserializable(&Message::CloseProofReq(CloseProofReq {
            epoch: ConsensusEpoch::new(7),
        }));
        assert_deserializable(&Message::CloseProofReply(proof()));
    }
    #[test]
    fn truncated_or_overlong_proof_is_rejected() {
        let mut bytes = Vec::new();
        proof().serialize(&mut bytes).unwrap();
        assert!(CloseProofReply::deserialize(&bytes[..bytes.len() - 1]).is_err());
        bytes.push(0);
        assert!(CloseProofReply::deserialize(&bytes).is_err());
    }
    /* Test helpers */
    fn proof() -> CloseProofReply {
        CloseProofReply {
            proposal: EpochProp::new_test_instance(),
            votes: vec![Vote::new_in_epoch_at(
                &PrivateKey::from(1),
                VoteKind::Final,
                ConsensusEpoch::close_round(ConsensusEpoch::new(1), 2),
                UnixMillisTimestamp::ZERO,
                vec![BlockHash::from(8)],
            )],
        }
    }
}
