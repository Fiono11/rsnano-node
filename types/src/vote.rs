use std::{io::Read, time::Duration};

use super::{
    Account, Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature, UnixMillisTimestamp,
    VoteTimestamp,
};
use crate::{DeserializationError, SignatureError};

#[derive(Copy, Clone, PartialEq, Eq, Debug, EnumCount, EnumIter)]
pub enum VoteDelivery {
    Direct,
    Forwarded,
    Replayed,
}

impl VoteDelivery {
    pub fn as_str(&self) -> &'static str {
        match self {
            VoteDelivery::Direct => "direct",
            VoteDelivery::Forwarded => "forwarded",
            VoteDelivery::Replayed => "replayed",
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

#[derive(PartialEq, Eq, Debug, Clone, Copy, Hash)]
pub enum VoteType {
    NonFinal,
    Final,
    #[cfg(feature = "rai_protocol")]
    First,
    #[cfg(feature = "rai_protocol")]
    Timeout,
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

    // Signature of timestamp + block hashes
    pub signature: Signature,

    // The hashes for which this vote directly covers
    pub hashes: Vec<BlockHash>,

    #[cfg(feature = "rai_protocol")]
    pub epoch: u64,
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
            #[cfg(feature = "rai_protocol")]
            epoch: 1,
        }
    }

    pub fn new_final(key: &PrivateKey, hashes: Vec<BlockHash>) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        Self::new(key, Self::TIMESTAMP_MAX, Self::DURATION_MAX, hashes)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_rai(
        key: &PrivateKey,
        epoch: u64,
        vote_type: VoteType,
        hashes: Vec<BlockHash>,
    ) -> Self {
        assert!(epoch > 0, "RAI network epochs start at one");
        let (timestamp, duration) = match vote_type {
            VoteType::Timeout => (UnixMillisTimestamp::ZERO, 0),
            VoteType::First => (UnixMillisTimestamp::ZERO, 1),
            VoteType::NonFinal => (UnixMillisTimestamp::now(), 9),
            VoteType::Final => (Self::TIMESTAMP_MAX, Self::DURATION_MAX),
        };
        let mut vote = Self::new(key, timestamp, duration, hashes);
        vote.epoch = epoch;
        vote.signature = key.sign(vote.hash().as_bytes());
        vote
    }

    pub fn new(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        hashes: Vec<BlockHash>,
    ) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        let mut result = Self {
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            hashes,
            #[cfg(feature = "rai_protocol")]
            epoch: 1,
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

    #[cfg(feature = "rai_protocol")]
    pub fn is_first(&self) -> bool {
        self.timestamp.rai_vote_type() == VoteType::First
    }

    #[cfg(feature = "rai_protocol")]
    pub fn is_timeout(&self) -> bool {
        self.timestamp.rai_vote_type() == VoteType::Timeout
    }

    #[cfg(feature = "rai_protocol")]
    pub const fn vote_type(&self) -> VoteType {
        self.timestamp.rai_vote_type()
    }

    #[cfg(feature = "rai_protocol")]
    pub const fn epoch(&self) -> u64 {
        self.epoch
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
        #[cfg(feature = "rai_protocol")]
        {
            builder = builder.update(self.epoch.to_le_bytes());
        }
        builder.build()
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let voter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut buffer = [0; 8];
        bytes.read_exact(&mut buffer)?;
        let timestamp = VoteTimestamp::from_le_bytes(buffer);
        #[cfg(feature = "rai_protocol")]
        let epoch = {
            bytes.read_exact(&mut buffer)?;
            let epoch = u64::from_le_bytes(buffer);
            if epoch == 0 {
                return Err(DeserializationError::InvalidData);
            }
            epoch
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
            #[cfg(feature = "rai_protocol")]
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
        + if cfg!(feature = "rai_protocol") { std::mem::size_of::<u64>() } else { 0 }
        + (BlockHash::SERIALIZED_SIZE * count)
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)?;
        writer.write_all(&self.timestamp.to_le_bytes())?;
        #[cfg(feature = "rai_protocol")]
        writer.write_all(&self.epoch.to_le_bytes())?;
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
            && {
                #[cfg(feature = "rai_protocol")]
                {
                    self.epoch == other.epoch
                }
                #[cfg(not(feature = "rai_protocol"))]
                {
                    true
                }
            }
    }
}

impl Eq for Vote {}

pub struct TestVoteBuilder {
    key: PrivateKey,
    timestamp: UnixMillisTimestamp,
    duration: u8,
    is_final: bool,
    hashes: Vec<BlockHash>,
    #[cfg(feature = "rai_protocol")]
    epoch: u64,
}

impl TestVoteBuilder {
    fn new() -> Self {
        Self {
            key: PrivateKey::from(42),
            timestamp: UnixMillisTimestamp::new(1),
            duration: 2,
            is_final: false,
            hashes: vec![BlockHash::from(5)],
            #[cfg(feature = "rai_protocol")]
            epoch: 1,
        }
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

    #[cfg(feature = "rai_protocol")]
    pub fn epoch(mut self, epoch: u64) -> Self {
        self.epoch = epoch;
        self
    }

    pub fn finish(self) -> Vote {
        if self.is_final {
            let mut vote = Vote::new_final(&self.key, self.hashes);
            #[cfg(feature = "rai_protocol")]
            if self.epoch != vote.epoch {
                vote.epoch = self.epoch;
                vote.signature = self.key.sign(vote.hash().as_bytes());
            }
            vote
        } else {
            let mut vote = Vote::new(&self.key, self.timestamp, self.duration, self.hashes);
            #[cfg(feature = "rai_protocol")]
            if self.epoch != vote.epoch {
                vote.epoch = self.epoch;
                vote.signature = self.key.sign(vote.hash().as_bytes());
            }
            vote
        }
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use super::*;

    #[test]
    fn encodes_all_vote_phases() {
        let key = PrivateKey::from(7);
        assert_eq!(
            Vote::new(&key, UnixMillisTimestamp::ZERO, 0, vec![]).vote_type(),
            VoteType::Timeout
        );
        assert_eq!(
            Vote::new(&key, UnixMillisTimestamp::ZERO, 1, vec![]).vote_type(),
            VoteType::First
        );
        assert_eq!(
            Vote::new(&key, UnixMillisTimestamp::new(16), 0, vec![]).vote_type(),
            VoteType::NonFinal
        );
        assert_eq!(Vote::new_final(&key, vec![]).vote_type(), VoteType::Final);
    }

    #[test]
    fn epoch_is_serialized_and_signed() {
        let vote = Vote::build_test_instance().epoch(9).finish();
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();
        let decoded = Vote::deserialize(&bytes).unwrap();
        assert_eq!(decoded, vote);
        assert_eq!(decoded.epoch(), 9);

        let mut changed = decoded;
        changed.epoch = 10;
        assert!(changed.validate().is_err());
    }

    #[test]
    fn rejects_epoch_zero_from_network() {
        let vote = Vote::new_test_instance();
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();
        bytes[Account::SERIALIZED_SIZE + Signature::SERIALIZED_SIZE + 8..][..8]
            .copy_from_slice(&0u64.to_le_bytes());
        assert!(Vote::deserialize(&bytes).is_err());
    }
}
