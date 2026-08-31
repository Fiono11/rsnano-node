use std::{io::Read, time::Duration};

use super::{
    Account, Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature, UnixMillisTimestamp,
};
#[cfg(not(feature = "rai_protocol"))]
use super::VoteTimestamp;
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
    #[cfg(not(feature = "rai_protocol"))]
    timestamp: VoteTimestamp,

    #[cfg(feature = "rai_protocol")]
    kind: RaiVoteKind,

    #[cfg(feature = "rai_protocol")]
    epoch: u64,

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
            #[cfg(not(feature = "rai_protocol"))]
            timestamp: 0.into(),
            #[cfg(feature = "rai_protocol")]
            kind: RaiVoteKind::First,
            #[cfg(feature = "rai_protocol")]
            epoch: 0,
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            hashes: Vec::new(),
        }
    }

    pub fn new_final(key: &PrivateKey, hashes: Vec<BlockHash>) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        Self::new(key, Self::TIMESTAMP_MAX, Self::DURATION_MAX, hashes)
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
            #[cfg(not(feature = "rai_protocol"))]
            timestamp: VoteTimestamp::new(timestamp, duration),
            #[cfg(feature = "rai_protocol")]
            kind: if timestamp == Self::TIMESTAMP_MAX && duration == Self::DURATION_MAX {
                RaiVoteKind::Final
            } else {
                // The normal Nano vote generator's first non-final vote maps to Kudzu's
                // combined FirstVote + NotarVote on the optimistic path.
                RaiVoteKind::First
            },
            #[cfg(feature = "rai_protocol")]
            epoch: 0,
            signature: Signature::new(),
            hashes,
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
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.timestamp.unix_timestamp()
        }
        #[cfg(feature = "rai_protocol")]
        {
            match self.kind {
                RaiVoteKind::First => UnixMillisTimestamp::ZERO,
                RaiVoteKind::Timeout => UnixMillisTimestamp::new(1),
                RaiVoteKind::Notarization => UnixMillisTimestamp::new(2),
                RaiVoteKind::Final => UnixMillisTimestamp::MAX,
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_rai(
        key: &PrivateKey,
        epoch: u64,
        kind: RaiVoteKind,
        hashes: Vec<BlockHash>,
    ) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        let mut result = Self {
            kind,
            epoch,
            voter: key.public_key(),
            signature: Signature::new(),
            hashes,
        };
        result.signature = key.sign(result.hash().as_bytes());
        result
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_kind(&self) -> RaiVoteKind {
        self.kind
    }

    pub fn is_final(&self) -> bool {
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.timestamp.is_final()
        }
        #[cfg(feature = "rai_protocol")]
        {
            self.kind == RaiVoteKind::Final
        }
    }

    pub fn duration_bits(&self) -> u8 {
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.timestamp.duration_bits()
        }
        #[cfg(feature = "rai_protocol")]
        {
            0
        }
    }

    pub fn duration(&self) -> Duration {
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.timestamp.duration()
        }
        #[cfg(feature = "rai_protocol")]
        {
            Duration::ZERO
        }
    }

    pub fn hash(&self) -> BlockHash {
        let mut builder = Blake2HashBuilder::new().update(HASH_PREFIX);

        for hash in &self.hashes {
            builder = builder.update(hash.as_bytes())
        }

        #[cfg(feature = "rai_protocol")]
        let builder = builder.update(self.epoch.to_le_bytes());

        #[cfg(not(feature = "rai_protocol"))]
        let bytes = self.timestamp.to_ne_bytes();
        #[cfg(feature = "rai_protocol")]
        let bytes = self.kind.to_le_bytes();

        builder.update(bytes).build()
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let voter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut buffer = [0; 8];
        bytes.read_exact(&mut buffer)?;
        #[cfg(not(feature = "rai_protocol"))]
        let timestamp = VoteTimestamp::from_le_bytes(buffer);
        #[cfg(feature = "rai_protocol")]
        let kind = RaiVoteKind::from_le_bytes(buffer);
        #[cfg(feature = "rai_protocol")]
        let epoch = {
            let mut buffer = [0; 8];
            bytes.read_exact(&mut buffer)?;
            u64::from_le_bytes(buffer)
        };
        let mut hashes = Vec::new();
        while !bytes.is_empty() && hashes.len() < Self::MAX_HASHES {
            hashes.push(BlockHash::deserialize(&mut bytes)?);
        }
        Ok(Self {
            #[cfg(not(feature = "rai_protocol"))]
            timestamp,
            #[cfg(feature = "rai_protocol")]
            kind,
            #[cfg(feature = "rai_protocol")]
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
        + if cfg!(feature = "rai_protocol") { std::mem::size_of::<u64>() } else { 0 }
        + (BlockHash::SERIALIZED_SIZE * count)
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)?;
        #[cfg(not(feature = "rai_protocol"))]
        writer.write_all(&self.timestamp.to_le_bytes())?;
        #[cfg(feature = "rai_protocol")]
        writer.write_all(&self.kind.to_le_bytes())?;
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
        ({
            #[cfg(not(feature = "rai_protocol"))]
            {
                self.timestamp == other.timestamp
            }
            #[cfg(feature = "rai_protocol")]
            {
                self.kind == other.kind
            }
        }) && {
            #[cfg(feature = "rai_protocol")]
            {
                self.epoch == other.epoch
            }
            #[cfg(not(feature = "rai_protocol"))]
            {
                true
            }
        }
            && self.voter == other.voter
            && self.signature == other.signature
            && self.hashes == other.hashes
    }
}

impl Eq for Vote {}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiVoteKind {
    First,
    Timeout,
    Notarization,
    Final,
}

#[cfg(feature = "rai_protocol")]
impl RaiVoteKind {
    fn to_le_bytes(self) -> [u8; 8] {
        let raw = match self {
            Self::First => 0,
            Self::Timeout => 1,
            Self::Notarization => 2,
            Self::Final => u64::MAX,
        };
        raw.to_le_bytes()
    }

    fn from_le_bytes(bytes: [u8; 8]) -> Self {
        match u64::from_le_bytes(bytes) {
            0 => Self::First,
            1 => Self::Timeout,
            u64::MAX => Self::Final,
            _ => Self::Notarization,
        }
    }
}

pub struct TestVoteBuilder {
    key: PrivateKey,
    timestamp: UnixMillisTimestamp,
    duration: u8,
    is_final: bool,
    hashes: Vec<BlockHash>,
}

impl TestVoteBuilder {
    fn new() -> Self {
        Self {
            key: PrivateKey::from(42),
            timestamp: UnixMillisTimestamp::new(1),
            duration: 2,
            is_final: false,
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

    pub fn blocks(mut self, hashes: impl IntoIterator<Item = BlockHash>) -> Self {
        self.hashes = hashes.into_iter().collect();
        self
    }

    pub fn finish(self) -> Vote {
        if self.is_final {
            Vote::new_final(&self.key, self.hashes)
        } else {
            Vote::new(&self.key, self.timestamp, self.duration, self.hashes)
        }
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use super::*;

    #[test]
    fn rai_vote_kind_and_epoch_are_signed_and_serialized() {
        let key = PrivateKey::from(7);
        let vote = Vote::new_rai(
            &key,
            42,
            RaiVoteKind::Notarization,
            vec![BlockHash::from(9)],
        );
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();
        let decoded = Vote::deserialize(&bytes).unwrap();

        assert_eq!(decoded.epoch(), 42);
        assert_eq!(decoded.rai_kind(), RaiVoteKind::Notarization);
        assert_eq!(decoded, vote);
        assert!(decoded.validate().is_ok());
    }

    #[test]
    fn rai_vote_kinds_are_explicit() {
        let key = PrivateKey::from(7);
        for kind in [
            RaiVoteKind::First,
            RaiVoteKind::Timeout,
            RaiVoteKind::Notarization,
            RaiVoteKind::Final,
        ] {
            let vote = Vote::new_rai(&key, 3, kind, vec![BlockHash::from(9)]);
            assert_eq!(vote.rai_kind(), kind);
        }
    }
}
