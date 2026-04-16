use std::{io::Read, time::Duration};

use super::{
    Account, Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature, UnixMillisTimestamp,
    VoteTimestamp,
};
use crate::{DeserializationError, SignatureError};

#[derive(Copy, Clone, PartialEq, Eq, Debug, EnumCount, EnumIter)]
pub enum VoteSource {
    Live,
    Rebroadcast,
    Cache,
}

impl VoteSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            VoteSource::Live => "live",
            VoteSource::Rebroadcast => "rebroadcast",
            VoteSource::Cache => "cache",
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

    #[cfg(feature = "ledger_snapshots")]
    pub epoch: u32,

    // Account that's voting
    pub voter: PublicKey,

    // Signature of timestamp + block hashes
    pub signature: Signature,

    // The hashes for which this vote directly covers
    pub hashes: Vec<BlockHash>,
}

static HASH_PREFIX: &str = "vote ";

impl Vote {
    pub const MAX_HASHES: usize = 255;
    pub fn null() -> Self {
        Self {
            timestamp: 0.into(),
            #[cfg(feature = "ledger_snapshots")]
            epoch: 0,
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            hashes: Vec::new(),
        }
    }

    #[cfg(feature = "ledger_snapshots")]
    pub fn new_final(key: &PrivateKey, epoch: u32, hashes: Vec<BlockHash>) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        Self::new(key, epoch, Self::TIMESTAMP_MAX, Self::DURATION_MAX, hashes)
    }

    #[cfg(not(feature = "ledger_snapshots"))]
    pub fn new_final(key: &PrivateKey, hashes: Vec<BlockHash>) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        Self::new(key, Self::TIMESTAMP_MAX, Self::DURATION_MAX, hashes)
    }

    #[cfg(feature = "ledger_snapshots")]
    pub fn new(
        priv_key: &PrivateKey,
        epoch: u32,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        hashes: Vec<BlockHash>,
    ) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        let mut result = Self {
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            epoch,
            signature: Signature::new(),
            hashes,
        };
        result.signature = priv_key.sign(result.hash().as_bytes());
        result
    }

    #[cfg(not(feature = "ledger_snapshots"))]
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
        };
        result.signature = priv_key.sign(result.hash().as_bytes());
        result
    }

    pub fn new_test_instance() -> Self {
        Self::build_test_instance().finish()
    }

    pub fn new_for_test(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        hashes: Vec<BlockHash>,
    ) -> Self {
        #[cfg(feature = "ledger_snapshots")]
        {
            Self::new(priv_key, 1, timestamp, duration, hashes)
        }

        #[cfg(not(feature = "ledger_snapshots"))]
        {
            Self::new(priv_key, timestamp, duration, hashes)
        }
    }

    pub fn new_final_for_test(key: &PrivateKey, hashes: Vec<BlockHash>) -> Self {
        #[cfg(feature = "ledger_snapshots")]
        {
            Self::new_final(key, 1, hashes)
        }

        #[cfg(not(feature = "ledger_snapshots"))]
        {
            Self::new_final(key, hashes)
        }
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

    pub fn duration_bits(&self) -> u8 {
        self.timestamp.duration_bits()
    }

    pub fn duration(&self) -> Duration {
        self.timestamp.duration()
    }

    pub fn epoch(&self) -> u32 {
        #[cfg(feature = "ledger_snapshots")]
        {
            self.epoch
        }

        #[cfg(not(feature = "ledger_snapshots"))]
        {
            0
        }
    }

    pub fn hash(&self) -> BlockHash {
        let mut builder = Blake2HashBuilder::new().update(HASH_PREFIX);

        #[cfg(feature = "ledger_snapshots")]
        {
            builder = builder.update(self.epoch.to_le_bytes());
        }

        for hash in &self.hashes {
            builder = builder.update(hash.as_bytes())
        }

        builder.update(self.timestamp.to_ne_bytes()).build()
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let voter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut buffer = [0; 8];
        bytes.read_exact(&mut buffer)?;
        let timestamp = VoteTimestamp::from_le_bytes(buffer);
        #[cfg(feature = "ledger_snapshots")]
        let epoch = {
            let mut buffer = [0; 4];
            bytes.read_exact(&mut buffer)?;
            u32::from_le_bytes(buffer)
        };
        let mut hashes = Vec::new();
        while !bytes.is_empty() && hashes.len() < Self::MAX_HASHES {
            hashes.push(BlockHash::deserialize(&mut bytes)?);
        }
        Ok(Self {
            timestamp,
            #[cfg(feature = "ledger_snapshots")]
            epoch,
            voter,
            signature,
            hashes,
        })
    }

    pub fn validate(&self) -> Result<(), SignatureError> {
        self.voter.verify(self.hash().as_bytes(), &self.signature)
    }

    pub const fn serialized_size(count: usize) -> usize {
        Account::SERIALIZED_SIZE
        + Signature::SERIALIZED_SIZE
        + std::mem::size_of::<u64>() // timestamp
        + {
            #[cfg(feature = "ledger_snapshots")]
            {
                std::mem::size_of::<u32>()
            }
            #[cfg(not(feature = "ledger_snapshots"))]
            {
                0
            }
        }
        + (BlockHash::SERIALIZED_SIZE * count)
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)?;
        writer.write_all(&self.timestamp.to_le_bytes())?;
        #[cfg(feature = "ledger_snapshots")]
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
            && self.epoch() == other.epoch()
            && self.voter == other.voter
            && self.signature == other.signature
            && self.hashes == other.hashes
    }
}

impl Eq for Vote {}

pub struct TestVoteBuilder {
    key: PrivateKey,
    timestamp: UnixMillisTimestamp,
    duration: u8,
    is_final: bool,
    epoch: u32,
    hashes: Vec<BlockHash>,
}

impl TestVoteBuilder {
    fn new() -> Self {
        Self {
            key: PrivateKey::from(42),
            timestamp: UnixMillisTimestamp::new(1),
            duration: 2,
            is_final: false,
            epoch: 0,
            hashes: vec![BlockHash::from(5)],
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

    pub fn epoch(mut self, epoch: u32) -> Self {
        self.epoch = epoch;
        self
    }

    pub fn blocks(mut self, hashes: impl IntoIterator<Item = BlockHash>) -> Self {
        self.hashes = hashes.into_iter().collect();
        self
    }

    pub fn finish(self) -> Vote {
        if self.is_final {
            #[cfg(feature = "ledger_snapshots")]
            {
                Vote::new_final(&self.key, self.epoch, self.hashes)
            }
            #[cfg(not(feature = "ledger_snapshots"))]
            {
                Vote::new_final(&self.key, self.hashes)
            }
        } else {
            #[cfg(feature = "ledger_snapshots")]
            {
                Vote::new(
                    &self.key,
                    self.epoch,
                    self.timestamp,
                    self.duration,
                    self.hashes,
                )
            }
            #[cfg(not(feature = "ledger_snapshots"))]
            {
                Vote::new(&self.key, self.timestamp, self.duration, self.hashes)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vote_roundtrip() {
        let vote = Vote::new_for_test(
            &PrivateKey::from(1),
            UnixMillisTimestamp::new(1234),
            7,
            vec![BlockHash::from(1), BlockHash::from(2)],
        );

        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();
        let deserialized = Vote::deserialize(&bytes).unwrap();

        assert_eq!(deserialized, vote);
    }

    #[cfg(feature = "ledger_snapshots")]
    #[test]
    fn epoch_is_included_in_roundtrip() {
        let vote = Vote::new(
            &PrivateKey::from(2),
            77,
            UnixMillisTimestamp::new(999),
            1,
            vec![BlockHash::from(123)],
        );

        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();
        let deserialized = Vote::deserialize(&bytes).unwrap();

        assert_eq!(deserialized.epoch(), 77);
        assert_eq!(deserialized.hash(), vote.hash());
    }
}
