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

/// RAI election statements. First votes also notarize their candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum VoteKind {
    First = 0,
    Notarize = 1,
    Final = 2,
    /// First-vote abstention; hashes identify elections, not supported candidates.
    FirstTimeout = 3,
    /// Timeout notarization after a FIRST vote; does not occupy the FIRST position.
    Timeout = 4,
}

#[derive(Clone, Debug)]
pub struct Vote {
    #[cfg(feature = "rai_protocol")]
    pub kind: VoteKind,
    pub epoch: crate::ConsensusEpoch,
    timestamp: VoteTimestamp,

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
            epoch: 0,
            #[cfg(feature = "rai_protocol")]
            kind: VoteKind::First,
            timestamp: 0.into(),
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
        Self::new_in_epoch(priv_key, timestamp, duration, hashes, 0)
    }

    pub fn new_in_epoch(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        hashes: Vec<BlockHash>,
        epoch: crate::ConsensusEpoch,
    ) -> Self {
        assert!(cfg!(feature = "rai_protocol") || epoch == 0);
        assert!(hashes.len() <= Self::MAX_HASHES);
        let mut result = Self {
            #[cfg(feature = "rai_protocol")]
            kind: if VoteTimestamp::new(timestamp, duration).is_final() {
                VoteKind::Final
            } else {
                VoteKind::First
            },
            epoch,
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            hashes,
        };
        result.signature = priv_key.sign(result.hash().as_bytes());
        result
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_with_kind(
        key: &PrivateKey,
        hashes: Vec<BlockHash>,
        epoch: u64,
        kind: VoteKind,
    ) -> Self {
        let (timestamp, duration) = if kind == VoteKind::Final {
            (Self::TIMESTAMP_MAX, Self::DURATION_MAX)
        } else {
            // These statements are immutable within an epoch. Stable timestamps
            // allow ordinary duplicate filtering to recognize retransmissions.
            (UnixMillisTimestamp::new(0), 9)
        };
        assert!(hashes.len() <= Self::MAX_HASHES);
        let mut vote = Self {
            kind,
            epoch,
            voter: key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            hashes,
        };
        vote.signature = key.sign(vote.hash().as_bytes());
        vote
    }

    pub fn kind(&self) -> VoteKind {
        #[cfg(feature = "rai_protocol")]
        {
            self.kind
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            if self.is_final() {
                VoteKind::Final
            } else {
                VoteKind::First
            }
        }
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
        #[cfg(feature = "rai_protocol")]
        {
            self.kind == VoteKind::Final
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.timestamp.is_final()
        }
    }

    pub fn duration_bits(&self) -> u8 {
        self.timestamp.duration_bits()
    }

    pub fn duration(&self) -> Duration {
        self.timestamp.duration()
    }

    pub fn hash(&self) -> BlockHash {
        let mut builder = Blake2HashBuilder::new().update(HASH_PREFIX);

        #[cfg(feature = "rai_protocol")]
        {
            builder = builder
                .update(b"rai-kudzu-vote-v3")
                .update(self.epoch.to_le_bytes())
                .update([self.kind as u8]);
        }
        for hash in &self.hashes {
            builder = builder.update(hash.as_bytes())
        }

        builder.update(self.timestamp.to_ne_bytes()).build()
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let epoch = if cfg!(feature = "rai_protocol") {
            let mut buffer = [0; 8];
            bytes.read_exact(&mut buffer)?;
            u64::from_le_bytes(buffer)
        } else {
            0
        };
        #[cfg(feature = "rai_protocol")]
        let kind = {
            let mut tag = [0];
            bytes.read_exact(&mut tag)?;
            match tag[0] {
                0 => VoteKind::First,
                1 => VoteKind::Notarize,
                2 => VoteKind::Final,
                3 => VoteKind::FirstTimeout,
                4 => VoteKind::Timeout,
                _ => return Err(DeserializationError::InvalidData),
            }
        };
        let voter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut buffer = [0; 8];
        bytes.read_exact(&mut buffer)?;
        let timestamp = VoteTimestamp::from_le_bytes(buffer);
        #[cfg(feature = "rai_protocol")]
        if (kind == VoteKind::Final) != timestamp.is_final() {
            return Err(DeserializationError::InvalidData);
        }
        let mut hashes = Vec::new();
        while !bytes.is_empty() && hashes.len() < Self::MAX_HASHES {
            hashes.push(BlockHash::deserialize(&mut bytes)?);
        }
        Ok(Self {
            #[cfg(feature = "rai_protocol")]
            kind,
            epoch,
            timestamp,
            voter,
            signature,
            hashes,
        })
    }

    pub fn validate(&self) -> Result<(), SignatureError> {
        #[cfg(feature = "rai_protocol")]
        if self.is_final() != self.timestamp.is_final() {
            return Err(SignatureError {});
        }
        self.voter.verify(self.hash().as_bytes(), &self.signature)
    }

    pub const fn serialized_size(count: usize) -> usize {
        (if cfg!(feature = "rai_protocol") { 9 } else { 0 }) + Account::SERIALIZED_SIZE
        + Signature::SERIALIZED_SIZE
        + std::mem::size_of::<u64>() // timestamp
        + (BlockHash::SERIALIZED_SIZE * count)
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        #[cfg(feature = "rai_protocol")]
        writer.write_all(&self.epoch.to_le_bytes())?;
        #[cfg(feature = "rai_protocol")]
        writer.write_all(&[self.kind as u8])?;
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)?;
        writer.write_all(&self.timestamp.to_le_bytes())?;
        for hash in &self.hashes {
            hash.serialize(writer)?;
        }
        Ok(())
    }
}

impl PartialEq for Vote {
    fn eq(&self, other: &Self) -> bool {
        self.kind() == other.kind()
            && self.epoch == other.epoch
            && self.timestamp == other.timestamp
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
    fn kudzu_vote_kinds_are_signed_and_roundtrip() {
        let key = PrivateKey::from(42);
        for kind in [
            VoteKind::First,
            VoteKind::Notarize,
            VoteKind::Final,
            VoteKind::FirstTimeout,
            VoteKind::Timeout,
        ] {
            let vote = Vote::new_with_kind(&key, vec![BlockHash::from(1)], 7, kind);
            let mut bytes = Vec::new();
            vote.serialize(&mut bytes).unwrap();
            assert_eq!(bytes.len(), Vote::serialized_size(1));
            assert_eq!(Vote::deserialize(&bytes).unwrap(), vote);
            assert!(vote.validate().is_ok());
            assert_eq!(vote.is_final(), kind == VoteKind::Final);
            if kind != VoteKind::Final {
                // Immutable phase statements must not look fresh on each retry.
                assert_eq!(vote.timestamp().as_u64(), 0);
            }
            let replay = Vote::new_with_kind(&key, vec![BlockHash::from(1)], 7, kind);
            assert_eq!(replay, vote);
            let mut changed = vote.clone();
            changed.kind = if kind == VoteKind::First {
                VoteKind::Notarize
            } else {
                VoteKind::First
            };
            assert!(changed.validate().is_err());
            bytes[8] = 255;
            assert!(Vote::deserialize(&bytes).is_err());
        }
    }

    #[test]
    fn rai_epoch_is_signed_and_roundtrips() {
        let key = PrivateKey::from(42);
        let vote = Vote::new_in_epoch(
            &key,
            Vote::TIMESTAMP_MAX,
            Vote::DURATION_MAX,
            vec![BlockHash::from(1)],
            7,
        );
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();
        assert_eq!(bytes.len(), Vote::serialized_size(1));
        assert_eq!(vote, Vote::deserialize(&bytes).unwrap());
        assert!(vote.validate().is_ok());
        let mut changed = vote.clone();
        changed.epoch = 8;
        assert!(changed.validate().is_err());
    }
}
