use std::{io::Read, time::Duration};

use super::{
    Account, Blake2HashBuilder, BlockHash, ConsensusEpoch, PrivateKey, PublicKey, Signature,
    UnixMillisTimestamp, VoteTimestamp,
};
use crate::{DeserializationError, SignatureError};

/// Kudzu vote kinds. A kind is carried in the 4 duration bits of the vote
/// timestamp, so the wire format and the signed payload stay unchanged.
/// Every legacy non-final vote reads as a First vote.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, EnumCount, EnumIter)]
pub enum VoteKind {
    /// FirstVote: the one-shot vote for the block proposed at this slot. Contains a notarization vote.
    First,
    /// NotarVote: cast on a second look or after a timeout termination
    Notar,
    /// NotarVote for the timeout block
    Timeout,
    /// RAI: the timeout vote of a replica that never proposed in this
    /// instance and never will, it has left the instance's epoch
    Abstain,
    /// FinalVote
    Final,
}

impl VoteKind {
    const ABSTAIN_BITS: u8 = 0xC;
    const NOTAR_BITS: u8 = 0xD;
    const TIMEOUT_BITS: u8 = 0xE;

    pub fn duration_bits(self) -> u8 {
        match self {
            VoteKind::First => 0x9, /*8192ms, the legacy non-final duration*/
            VoteKind::Notar => Self::NOTAR_BITS,
            VoteKind::Timeout => Self::TIMEOUT_BITS,
            VoteKind::Abstain => Self::ABSTAIN_BITS,
            VoteKind::Final => Vote::DURATION_MAX,
        }
    }

    fn from_timestamp(timestamp: VoteTimestamp) -> Self {
        if timestamp.is_final() {
            VoteKind::Final
        } else {
            match timestamp.duration_bits() {
                Self::NOTAR_BITS => VoteKind::Notar,
                Self::TIMEOUT_BITS => VoteKind::Timeout,
                Self::ABSTAIN_BITS => VoteKind::Abstain,
                _ => VoteKind::First,
            }
        }
    }

    pub fn is_final(self) -> bool {
        self == VoteKind::Final
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            VoteKind::First => "first",
            VoteKind::Notar => "notar",
            VoteKind::Timeout => "timeout",
            VoteKind::Abstain => "abstain",
            VoteKind::Final => "final",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, EnumCount, EnumIter)]
pub enum VoteDelivery {
    Direct,
    Forwarded,
    Replayed,
    /// Kudzu: a retained vote handed over on request as certificate evidence.
    /// It is only applied to the active elections it names; it is neither
    /// cached for later elections nor rebroadcast.
    Evidence,
}

impl VoteDelivery {
    pub fn as_str(&self) -> &'static str {
        match self {
            VoteDelivery::Direct => "direct",
            VoteDelivery::Forwarded => "forwarded",
            VoteDelivery::Replayed => "replayed",
            VoteDelivery::Evidence => "evidence",
        }
    }
}

#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoteError {
    /// Vote is not signed correctly
    Invalid,

    /// Vote does not have the highest timestamp, it's a replay
    Replay,

    /// Vote has the highest timestamp
    Vote,

    /// Vote is late, the election is already confirmed and present in the recently confirmed set
    Late,

    /// Unknown if replay or vote
    Indeterminate,

    /// Vote is valid, but got ingored (e.g. due to cooldown)
    Ignored,
}

impl VoteError {
    pub fn as_str(&self) -> &'static str {
        match self {
            VoteError::Vote => "vote",
            VoteError::Late => "late",
            VoteError::Replay => "replay",
            VoteError::Indeterminate => "indeterminate",
            VoteError::Ignored => "ignored",
            VoteError::Invalid => "invalid",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Vote {
    timestamp: VoteTimestamp,

    // Account that's voting
    pub voter: PublicKey,

    // Signature of timestamp + block hashes (+ epoch under the RAI protocol)
    pub signature: Signature,

    // The hashes for which this vote directly covers
    pub hashes: Vec<BlockHash>,

    /// RAI: the consensus epoch this vote belongs to. Part of the signed
    /// payload and of the wire format under the `rai_protocol` feature only;
    /// the legacy protocol has the single implicit epoch zero.
    pub epoch: ConsensusEpoch,
}

static HASH_PREFIX: &str = "vote ";

impl Vote {
    pub const MAX_HASHES: usize = 255;
    pub fn null() -> Self {
        Self {
            timestamp: 0.into(),
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            hashes: Vec::new(),
            epoch: ConsensusEpoch::ZERO,
        }
    }

    pub fn new_final(key: &PrivateKey, hashes: Vec<BlockHash>) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        Self::new(key, Self::TIMESTAMP_MAX, Self::DURATION_MAX, hashes)
    }

    pub fn new_of_kind(key: &PrivateKey, kind: VoteKind, hashes: Vec<BlockHash>) -> Self {
        Self::new_of_kind_at(key, kind, UnixMillisTimestamp::now(), hashes)
    }

    pub fn new_of_kind_at(
        key: &PrivateKey,
        kind: VoteKind,
        timestamp: UnixMillisTimestamp,
        hashes: Vec<BlockHash>,
    ) -> Self {
        Self::new_in_epoch_at(key, kind, ConsensusEpoch::ZERO, timestamp, hashes)
    }

    /// RAI: a vote of the given kind for one consensus epoch
    pub fn new_in_epoch(
        key: &PrivateKey,
        kind: VoteKind,
        epoch: ConsensusEpoch,
        hashes: Vec<BlockHash>,
    ) -> Self {
        Self::new_in_epoch_at(key, kind, epoch, UnixMillisTimestamp::now(), hashes)
    }

    pub fn new_in_epoch_at(
        key: &PrivateKey,
        kind: VoteKind,
        epoch: ConsensusEpoch,
        timestamp: UnixMillisTimestamp,
        hashes: Vec<BlockHash>,
    ) -> Self {
        let timestamp = if kind.is_final() {
            Self::TIMESTAMP_MAX
        } else {
            timestamp
        };
        Self::sign(key, timestamp, kind.duration_bits(), epoch, hashes)
    }

    pub fn new(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        hashes: Vec<BlockHash>,
    ) -> Self {
        Self::sign(priv_key, timestamp, duration, ConsensusEpoch::ZERO, hashes)
    }

    fn sign(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        epoch: ConsensusEpoch,
        hashes: Vec<BlockHash>,
    ) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        let mut result = Self {
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            hashes,
            epoch,
        };
        result.signature = priv_key.sign(result.hash().as_bytes());
        result
    }

    pub fn new_test_instance() -> Self {
        Self::build_test_instance().finish()
    }

    pub fn build_test_instance() -> TestVoteBuilder {
        TestVoteBuilder::new()
    }

    pub const DURATION_MAX: u8 = 0x0F;
    pub const TIMESTAMP_MAX: UnixMillisTimestamp = UnixMillisTimestamp::new(0xFFFF_FFFF_FFFF_FFF0);
    pub const TIMESTAMP_MIN: UnixMillisTimestamp = UnixMillisTimestamp::new(0x0000_0000_0000_0010);

    pub fn timestamp(&self) -> UnixMillisTimestamp {
        self.timestamp.unix_timestamp()
    }

    pub fn is_final(&self) -> bool {
        self.timestamp.is_final()
    }

    pub fn kind(&self) -> VoteKind {
        VoteKind::from_timestamp(self.timestamp)
    }

    pub fn duration_bits(&self) -> u8 {
        self.timestamp.duration_bits()
    }

    pub fn duration(&self) -> Duration {
        self.timestamp.duration()
    }

    pub fn hash(&self) -> BlockHash {
        let mut builder = Blake2HashBuilder::new().update(HASH_PREFIX);

        for hash in &self.hashes {
            builder = builder.update(hash.as_bytes())
        }

        builder = builder.update(self.timestamp.to_ne_bytes());
        if Self::EPOCH_ON_WIRE {
            builder = builder.update(self.epoch.as_u64().to_le_bytes());
        }
        builder.build()
    }

    /// RAI: the epoch is signed and serialized after the timestamp. The legacy
    /// wire format has no epoch, every legacy vote is in epoch zero.
    const EPOCH_ON_WIRE: bool = cfg!(feature = "rai_protocol");

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let voter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut buffer = [0; 8];
        bytes.read_exact(&mut buffer)?;
        let timestamp = VoteTimestamp::from_le_bytes(buffer);
        let epoch = if Self::EPOCH_ON_WIRE {
            ConsensusEpoch::deserialize(&mut bytes)?
        } else {
            ConsensusEpoch::ZERO
        };
        let mut hashes = Vec::new();
        while !bytes.is_empty() && hashes.len() < Self::MAX_HASHES {
            hashes.push(BlockHash::deserialize(&mut bytes)?);
        }
        Ok(Self {
            timestamp,
            voter,
            signature,
            hashes,
            epoch,
        })
    }

    pub fn validate(&self) -> Result<(), SignatureError> {
        self.voter.verify(self.hash().as_bytes(), &self.signature)
    }

    pub const fn serialized_size(count: usize) -> usize {
        Account::SERIALIZED_SIZE
        + Signature::SERIALIZED_SIZE
        + std::mem::size_of::<u64>() // timestamp
        + if Self::EPOCH_ON_WIRE { ConsensusEpoch::SERIALIZED_SIZE } else { 0 }
        + (BlockHash::SERIALIZED_SIZE * count)
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)?;
        writer.write_all(&self.timestamp.to_le_bytes())?;
        if Self::EPOCH_ON_WIRE {
            self.epoch.serialize(writer)?;
        }
        for hash in &self.hashes {
            hash.serialize(writer)?;
        }
        Ok(())
    }
}

impl PartialEq for Vote {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp == other.timestamp
            && self.voter == other.voter
            && self.signature == other.signature
            && self.hashes == other.hashes
            && self.epoch == other.epoch
    }
}

impl Eq for Vote {}

pub struct TestVoteBuilder {
    key: PrivateKey,
    timestamp: UnixMillisTimestamp,
    duration: u8,
    is_final: bool,
    hashes: Vec<BlockHash>,
    epoch: ConsensusEpoch,
}

impl TestVoteBuilder {
    fn new() -> Self {
        Self {
            key: PrivateKey::from(42),
            timestamp: UnixMillisTimestamp::new(1),
            duration: 2,
            is_final: false,
            hashes: vec![BlockHash::from(5)],
            epoch: ConsensusEpoch::ZERO,
        }
    }

    pub fn epoch(mut self, epoch: ConsensusEpoch) -> Self {
        self.epoch = epoch;
        self
    }

    pub fn voter_key(mut self, key: impl Into<PrivateKey>) -> Self {
        self.key = key.into();
        self
    }

    pub fn timestamp(mut self, ts: UnixMillisTimestamp) -> Self {
        self.timestamp = ts;
        self
    }

    pub fn final_vote(mut self) -> Self {
        self.is_final = true;
        self
    }

    pub fn blocks(mut self, hashes: impl IntoIterator<Item = BlockHash>) -> Self {
        self.hashes = hashes.into_iter().collect();
        self
    }

    pub fn finish(self) -> Vote {
        let (timestamp, duration) = if self.is_final {
            (Vote::TIMESTAMP_MAX, Vote::DURATION_MAX)
        } else {
            (self.timestamp, self.duration)
        };
        Vote::sign(&self.key, timestamp, duration, self.epoch, self.hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn legacy_votes_read_as_first_and_final_kinds() {
        let non_final = Vote::build_test_instance().finish();
        assert_eq!(non_final.kind(), VoteKind::First);

        let legacy_generator_vote = Vote::new(
            &PrivateKey::from(1),
            UnixMillisTimestamp::new(1000),
            0x9,
            vec![BlockHash::from(1)],
        );
        assert_eq!(legacy_generator_vote.kind(), VoteKind::First);

        let final_vote = Vote::build_test_instance().final_vote().finish();
        assert_eq!(final_vote.kind(), VoteKind::Final);
    }

    #[test]
    fn kind_survives_serialization_and_signing() {
        for kind in VoteKind::iter() {
            let vote = Vote::new_of_kind(&PrivateKey::from(1), kind, vec![BlockHash::from(1)]);
            assert_eq!(vote.kind(), kind);
            assert_eq!(vote.is_final(), kind.is_final());
            assert!(vote.validate().is_ok());

            let mut bytes = Vec::new();
            vote.serialize(&mut bytes).unwrap();
            let deserialized = Vote::deserialize(&bytes).unwrap();
            assert_eq!(deserialized.kind(), kind);
            assert_eq!(deserialized, vote);
        }
    }

    #[test]
    fn legacy_votes_are_in_epoch_zero() {
        let vote = Vote::new_test_instance();
        assert_eq!(vote.epoch, ConsensusEpoch::ZERO);
        assert!(vote.validate().is_ok());
    }

    /// RAI: the epoch is signed, a vote cannot be replayed into another epoch
    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epoch_is_signed_and_serialized() {
        let epoch = ConsensusEpoch::new(3);
        let vote = Vote::build_test_instance().epoch(epoch).finish();
        assert_eq!(vote.epoch, epoch);
        assert!(vote.validate().is_ok());

        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();
        assert_eq!(bytes.len(), Vote::serialized_size(vote.hashes.len()));
        let deserialized = Vote::deserialize(&bytes).unwrap();
        assert_eq!(deserialized.epoch, epoch);
        assert_eq!(deserialized, vote);

        let mut replayed = vote.clone();
        replayed.epoch = epoch.next();
        assert!(replayed.validate().is_err());

        let same_epoch = Vote::build_test_instance().epoch(epoch).finish();
        let other_epoch = Vote::build_test_instance().epoch(epoch.next()).finish();
        assert_ne!(same_epoch.hash(), other_epoch.hash());
    }

    /// The legacy wire format is unchanged: the epoch is not on the wire
    #[cfg(not(feature = "rai_protocol"))]
    #[test]
    fn epoch_is_not_on_the_legacy_wire() {
        let vote = Vote::build_test_instance()
            .epoch(ConsensusEpoch::new(3))
            .finish();
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();
        assert_eq!(
            bytes.len(),
            Account::SERIALIZED_SIZE + Signature::SERIALIZED_SIZE + 8 + BlockHash::SERIALIZED_SIZE
        );
        assert_eq!(
            Vote::deserialize(&bytes).unwrap().epoch,
            ConsensusEpoch::ZERO
        );
    }
}
