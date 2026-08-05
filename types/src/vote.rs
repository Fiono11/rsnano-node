use std::{io::Read, time::Duration};

use super::{
    Account, Blake2HashBuilder, BlockHash, PrivateKey, PublicKey, Signature, UnixMillisTimestamp,
    VoteTimestamp,
};
use crate::{DeserializationError, SignatureError};
#[cfg(feature = "rai_protocol")]
use crate::{QualifiedRoot, RaiEpoch};

#[cfg(feature = "rai_protocol")]
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RaiVotePhase {
    #[default]
    First = 0,
    Notar = 1,
    Final = 2,
}

#[cfg(feature = "rai_protocol")]
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RaiCommitteeScope {
    #[default]
    All = 0,
    Older = 1,
    Newer = 2,
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RaiSlotId {
    pub epoch: RaiEpoch,
    pub root: QualifiedRoot,
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RaiElectionId {
    Slot(RaiSlotId),
    CloseCut { epoch: RaiEpoch, round: u32 },
    CloseRecord { epoch: RaiEpoch, round: u32 },
}

#[cfg(feature = "rai_protocol")]
impl Default for RaiElectionId {
    fn default() -> Self {
        Self::Slot(RaiSlotId::default())
    }
}

#[cfg(feature = "rai_protocol")]
impl RaiElectionId {
    pub const SERIALIZED_SIZE: usize = 1 + 8 + QualifiedRoot::SERIALIZED_SIZE + 4;

    fn digest_bytes(&self) -> [u8; Self::SERIALIZED_SIZE] {
        let mut bytes = [0; Self::SERIALIZED_SIZE];
        self.serialize(&mut bytes.as_mut())
            .expect("serializing an election ID into a fixed buffer cannot fail");
        bytes
    }

    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        let (kind, epoch, root, round) = match self {
            Self::Slot(id) => (0, id.epoch, id.root.clone(), 0),
            Self::CloseCut { epoch, round } => (1, *epoch, QualifiedRoot::ZERO, *round),
            Self::CloseRecord { epoch, round } => (2, *epoch, QualifiedRoot::ZERO, *round),
        };
        writer.write_all(&[kind])?;
        writer.write_all(&epoch.number().to_le_bytes())?;
        root.serialize(writer)?;
        writer.write_all(&round.to_le_bytes())
    }

    fn deserialize<T: Read>(reader: &mut T) -> Result<Self, DeserializationError> {
        let kind = crate::read_u8(reader)?;
        let mut epoch_bytes = [0; 8];
        reader.read_exact(&mut epoch_bytes)?;
        let epoch = RaiEpoch::new(u64::from_le_bytes(epoch_bytes));
        let root = QualifiedRoot::deserialize(reader)?;
        let mut round_bytes = [0; 4];
        reader.read_exact(&mut round_bytes)?;
        let round = u32::from_le_bytes(round_bytes);
        match kind {
            0 if round == 0 => Ok(Self::Slot(RaiSlotId { epoch, root })),
            1 if root == QualifiedRoot::ZERO => Ok(Self::CloseCut { epoch, round }),
            2 if root == QualifiedRoot::ZERO => Ok(Self::CloseRecord { epoch, round }),
            _ => Err(DeserializationError::InvalidData),
        }
    }
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RaiVoteMetadata {
    pub election_id: RaiElectionId,
    pub phase: RaiVotePhase,
    pub epoch: RaiEpoch,
    pub governing_hash: BlockHash,
    pub scope: RaiCommitteeScope,
}

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
    timestamp: VoteTimestamp,

    #[cfg(feature = "rai_protocol")]
    pub metadata: RaiVoteMetadata,

    // Account that's voting
    pub voter: PublicKey,

    // Signature of timestamp + block hashes
    pub signature: Signature,

    // The hashes for which this vote directly covers
    pub hashes: Vec<BlockHash>,
}

#[cfg(not(feature = "rai_protocol"))]
static HASH_PREFIX: &str = "vote ";
#[cfg(feature = "rai_protocol")]
static RAI_HASH_PREFIX: &[u8] = b"RAI/Vote/v1";

impl Vote {
    pub const MAX_HASHES: usize = 255;
    pub fn null() -> Self {
        Self {
            timestamp: 0.into(),
            #[cfg(feature = "rai_protocol")]
            metadata: RaiVoteMetadata::default(),
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
        #[cfg(feature = "rai_protocol")]
        let phase = if timestamp == Self::TIMESTAMP_MAX {
            RaiVotePhase::Final
        } else {
            RaiVotePhase::First
        };
        #[cfg(feature = "rai_protocol")]
        return Self::new_rai(
            priv_key,
            timestamp,
            duration,
            hashes,
            RaiVoteMetadata {
                phase,
                ..Default::default()
            },
        );

        #[cfg(not(feature = "rai_protocol"))]
        {
            let mut result = Self {
                voter: priv_key.public_key(),
                timestamp: VoteTimestamp::new(timestamp, duration),
                signature: Signature::new(),
                hashes,
            };
            result.signature = priv_key.sign(result.hash().as_bytes());
            result
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_rai(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        hashes: Vec<BlockHash>,
        metadata: RaiVoteMetadata,
    ) -> Self {
        assert!(hashes.len() <= Self::MAX_HASHES);
        let mut result = Self {
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            hashes,
            metadata,
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

    pub fn duration_bits(&self) -> u8 {
        self.timestamp.duration_bits()
    }

    pub fn duration(&self) -> Duration {
        self.timestamp.duration()
    }

    pub fn hash(&self) -> BlockHash {
        #[cfg(feature = "rai_protocol")]
        {
            let mut builder = Blake2HashBuilder::new()
                .update(RAI_HASH_PREFIX)
                .update(self.metadata.election_id.digest_bytes())
                .update([self.metadata.phase as u8])
                .update(self.metadata.epoch.number().to_le_bytes())
                .update(self.metadata.governing_hash.as_bytes())
                .update([self.metadata.scope as u8])
                .update(self.timestamp.to_le_bytes());
            for hash in &self.hashes {
                builder = builder.update(hash.as_bytes());
            }
            return builder.build();
        }

        #[cfg(not(feature = "rai_protocol"))]
        {
            let mut builder = Blake2HashBuilder::new().update(HASH_PREFIX);
            for hash in &self.hashes {
                builder = builder.update(hash.as_bytes())
            }
            builder.update(self.timestamp.to_ne_bytes()).build()
        }
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let voter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut buffer = [0; 8];
        bytes.read_exact(&mut buffer)?;
        let timestamp = VoteTimestamp::from_le_bytes(buffer);
        #[cfg(feature = "rai_protocol")]
        let metadata = {
            let phase = match crate::read_u8(&mut bytes)? {
                0 => RaiVotePhase::First,
                1 => RaiVotePhase::Notar,
                2 => RaiVotePhase::Final,
                _ => return Err(DeserializationError::InvalidData),
            };
            let election_id = RaiElectionId::deserialize(&mut bytes)?;
            bytes.read_exact(&mut buffer)?;
            let epoch = RaiEpoch::new(u64::from_le_bytes(buffer));
            let governing_hash = BlockHash::deserialize(&mut bytes)?;
            let scope = match crate::read_u8(&mut bytes)? {
                0 => RaiCommitteeScope::All,
                1 => RaiCommitteeScope::Older,
                2 => RaiCommitteeScope::Newer,
                _ => return Err(DeserializationError::InvalidData),
            };
            RaiVoteMetadata {
                election_id,
                phase,
                epoch,
                governing_hash,
                scope,
            }
        };
        let mut hashes = Vec::new();
        while !bytes.is_empty() && hashes.len() < Self::MAX_HASHES {
            hashes.push(BlockHash::deserialize(&mut bytes)?);
        }
        Ok(Self {
            timestamp,
            #[cfg(feature = "rai_protocol")]
            metadata,
            voter,
            signature,
            hashes,
        })
    }

    pub fn validate(&self) -> Result<(), SignatureError> {
        self.voter.verify(self.hash().as_bytes(), &self.signature)
    }

    pub const fn serialized_size(count: usize) -> usize {
        let base = Account::SERIALIZED_SIZE
            + Signature::SERIALIZED_SIZE
            + std::mem::size_of::<u64>() // timestamp
            + (BlockHash::SERIALIZED_SIZE * count);
        #[cfg(feature = "rai_protocol")]
        return base + 1 + RaiElectionId::SERIALIZED_SIZE + 8 + BlockHash::SERIALIZED_SIZE + 1;
        #[cfg(not(feature = "rai_protocol"))]
        return base;
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)?;
        writer.write_all(&self.timestamp.to_le_bytes())?;
        #[cfg(feature = "rai_protocol")]
        {
            writer.write_all(&[self.metadata.phase as u8])?;
            self.metadata.election_id.serialize(writer)?;
            writer.write_all(&self.metadata.epoch.number().to_le_bytes())?;
            self.metadata.governing_hash.serialize(writer)?;
            writer.write_all(&[self.metadata.scope as u8])?;
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
            && {
                #[cfg(feature = "rai_protocol")]
                {
                    self.metadata == other.metadata
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

pub struct TestVoteBuilder {
    key: PrivateKey,
    timestamp: UnixMillisTimestamp,
    duration: u8,
    is_final: bool,
    hashes: Vec<BlockHash>,
    #[cfg(feature = "rai_protocol")]
    metadata: RaiVoteMetadata,
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
            metadata: RaiVoteMetadata::default(),
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
    pub fn rai_metadata(mut self, metadata: RaiVoteMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn finish(self) -> Vote {
        #[cfg(feature = "rai_protocol")]
        {
            let timestamp = if self.is_final {
                Vote::TIMESTAMP_MAX
            } else {
                self.timestamp
            };
            let duration = if self.is_final {
                Vote::DURATION_MAX
            } else {
                self.duration
            };
            return Vote::new_rai(&self.key, timestamp, duration, self.hashes, self.metadata);
        }
        #[cfg(not(feature = "rai_protocol"))]
        if self.is_final {
            Vote::new_final(&self.key, self.hashes)
        } else {
            Vote::new(&self.key, self.timestamp, self.duration, self.hashes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "rai_protocol")]
    fn rai_vote() -> Vote {
        Vote::new_rai(
            &PrivateKey::from(42),
            UnixMillisTimestamp::new(0x12340),
            3,
            vec![BlockHash::from(11), BlockHash::from(12)],
            RaiVoteMetadata {
                election_id: RaiElectionId::Slot(RaiSlotId {
                    epoch: RaiEpoch::new(7),
                    root: QualifiedRoot::new_test_instance(),
                }),
                phase: RaiVotePhase::Notar,
                epoch: RaiEpoch::new(7),
                governing_hash: BlockHash::from(10),
                scope: RaiCommitteeScope::Older,
            },
        )
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_vote_round_trip_and_signature_validation() {
        let vote = rai_vote();
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), Vote::serialized_size(2));
        let round_trip = Vote::deserialize(&bytes).unwrap();
        assert_eq!(round_trip, vote);
        assert!(round_trip.validate().is_ok());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn generated_vote_phase_matches_legacy_final_timestamp() {
        let key = PrivateKey::from(42);
        let hash = BlockHash::from(11);

        let first = Vote::new(&key, UnixMillisTimestamp::new(16), 0, vec![hash]);
        let final_vote = Vote::new_final(&key, vec![hash]);

        assert_eq!(first.metadata.phase, RaiVotePhase::First);
        assert_eq!(final_vote.metadata.phase, RaiVotePhase::Final);
        assert_eq!(final_vote.metadata.epoch, RaiEpoch::ZERO);
        assert_eq!(final_vote.metadata.governing_hash, BlockHash::ZERO);
        assert_eq!(final_vote.metadata.scope, RaiCommitteeScope::All);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_phase() {
        let mut vote = rai_vote();
        vote.metadata.phase = RaiVotePhase::Final;
        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_election_epoch() {
        let mut vote = rai_vote();
        let RaiElectionId::Slot(id) = &mut vote.metadata.election_id else {
            panic!("test vote must target a slot election");
        };
        id.epoch = RaiEpoch::new(id.epoch.number() + 1);

        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_epoch() {
        let mut vote = rai_vote();
        vote.metadata.epoch = RaiEpoch::new(8);
        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_governing_hash() {
        let mut vote = rai_vote();
        vote.metadata.governing_hash = BlockHash::from(99);
        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_committee_scope() {
        let mut vote = rai_vote();
        vote.metadata.scope = RaiCommitteeScope::Newer;
        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_every_candidate_hash() {
        for index in 0..2 {
            let mut vote = rai_vote();
            vote.hashes[index] = BlockHash::from(100 + index as u64);
            assert!(vote.validate().is_err());
        }
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_deserialization_rejects_unknown_phase_and_scope() {
        let vote = rai_vote();
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();
        let metadata_offset = PublicKey::SERIALIZED_SIZE + Signature::SERIALIZED_SIZE + 8;

        let mut invalid_phase = bytes.clone();
        invalid_phase[metadata_offset] = 3;
        assert!(Vote::deserialize(&invalid_phase).is_err());

        bytes[metadata_offset
            + 1
            + RaiElectionId::SERIALIZED_SIZE
            + 8
            + BlockHash::SERIALIZED_SIZE] = 3;
        assert!(Vote::deserialize(&bytes).is_err());
    }

    #[cfg(not(feature = "rai_protocol"))]
    #[test]
    fn legacy_serialization_layout_is_unchanged() {
        let vote = Vote::new(
            &PrivateKey::from(42),
            UnixMillisTimestamp::new(0x12340),
            3,
            vec![BlockHash::from(11), BlockHash::from(12)],
        );
        let mut actual = Vec::new();
        vote.serialize(&mut actual).unwrap();

        let mut expected = Vec::new();
        vote.voter.serialize(&mut expected).unwrap();
        vote.signature.serialize(&mut expected).unwrap();
        expected.extend_from_slice(&vote.timestamp.to_le_bytes());
        for hash in &vote.hashes {
            hash.serialize(&mut expected).unwrap();
        }

        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 32 + 64 + 8 + 2 * 32);
    }
}
