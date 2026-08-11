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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RaiVotePhase {
    #[default]
    First = 0,
    Notar = 1,
    Final = 2,
}

#[cfg(feature = "rai_protocol")]
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RaiVoteMetadata {
    pub election_id: RaiElectionId,
    pub phase: RaiVotePhase,
    pub epoch: RaiEpoch,
    pub scope: RaiCommitteeScope,
}

#[cfg(feature = "rai_protocol")]
impl RaiVoteMetadata {
    pub const SERIALIZED_SIZE: usize = 1 + RaiElectionId::SERIALIZED_SIZE + 8 + 1;

    /// Reserved context for a signed representative-discovery response. It is
    /// never admissible as slot or close-election evidence.
    pub fn is_discovery(&self) -> bool {
        self.election_id == RaiElectionId::default()
    }

    fn digest_bytes(&self) -> [u8; Self::SERIALIZED_SIZE] {
        let mut bytes = [0; Self::SERIALIZED_SIZE];
        self.serialize(&mut bytes.as_mut())
            .expect("serializing vote metadata into a fixed buffer cannot fail");
        bytes
    }

    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&[self.phase as u8])?;
        self.election_id.serialize(writer)?;
        writer.write_all(&self.epoch.number().to_le_bytes())?;
        writer.write_all(&[self.scope as u8])
    }

    fn deserialize<T: Read>(reader: &mut T) -> Result<Self, DeserializationError> {
        let phase = match crate::read_u8(reader)? {
            0 => RaiVotePhase::First,
            1 => RaiVotePhase::Notar,
            2 => RaiVotePhase::Final,
            _ => return Err(DeserializationError::InvalidData),
        };
        let election_id = RaiElectionId::deserialize(reader)?;
        let mut buffer = [0; 8];
        reader.read_exact(&mut buffer)?;
        let epoch = RaiEpoch::new(u64::from_le_bytes(buffer));
        let scope = match crate::read_u8(reader)? {
            0 => RaiCommitteeScope::All,
            1 => RaiCommitteeScope::Older,
            2 => RaiCommitteeScope::Newer,
            _ => return Err(DeserializationError::InvalidData),
        };
        Ok(Self {
            election_id,
            phase,
            epoch,
            scope,
        })
    }
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
    pub metadata: Vec<RaiVoteMetadata>,

    // Account that's voting
    pub voter: PublicKey,

    // Signature of the vote digest (including all RAI metadata/hash leaves)
    pub signature: Signature,

    // The hashes for which this vote directly covers
    pub hashes: Vec<BlockHash>,
}

#[cfg(not(feature = "rai_protocol"))]
static HASH_PREFIX: &str = "vote ";
#[cfg(feature = "rai_protocol")]
static RAI_HASH_PREFIX: &[u8] = b"RAI/VoteBatch/v3";

impl Vote {
    pub const MAX_HASHES: usize = 255;
    pub fn null() -> Self {
        Self {
            timestamp: 0.into(),
            #[cfg(feature = "rai_protocol")]
            metadata: Vec::new(),
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
        return Self::new_rai_batch(
            priv_key,
            timestamp,
            duration,
            hashes.into_iter().map(|hash| {
                (
                    RaiVoteMetadata {
                        phase,
                        ..Default::default()
                    },
                    hash,
                )
            }),
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
    /// Constructs and signs one RAI logical vote leaf.
    pub fn new_rai(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        hash: BlockHash,
        metadata: RaiVoteMetadata,
    ) -> Self {
        Self::new_rai_batch(priv_key, timestamp, duration, [(metadata, hash)])
    }

    #[cfg(feature = "rai_protocol")]
    /// Constructs one signed transport batch. Entries are sorted into their
    /// canonical `(metadata, hash)` order and exact duplicates are removed.
    pub fn new_rai_batch(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        entries: impl IntoIterator<Item = (RaiVoteMetadata, BlockHash)>,
    ) -> Self {
        let mut entries: Vec<_> = entries.into_iter().collect();
        entries.sort_unstable();
        entries.dedup();
        assert!(entries.len() <= Self::MAX_HASHES);
        let (metadata, hashes) = entries.into_iter().unzip();
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

    #[cfg(feature = "rai_protocol")]
    /// Iterates over the aligned logical leaves covered by this signature.
    pub fn rai_entries(
        &self,
    ) -> impl ExactSizeIterator<Item = (&RaiVoteMetadata, &BlockHash)> + '_ {
        self.metadata.iter().zip(&self.hashes)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_metadata(&self, index: usize) -> Option<&RaiVoteMetadata> {
        self.metadata.get(index)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_entry_count(&self) -> usize {
        self.hashes.len()
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
            let mut root_builder = Blake2HashBuilder::new()
                .update(self.timestamp.to_le_bytes())
                .update([self.hashes.len() as u8]);
            for (metadata, hash) in self.rai_entries() {
                root_builder = root_builder
                    .update(metadata.digest_bytes())
                    .update(hash.as_bytes());
            }
            let root = root_builder.build();
            return Blake2HashBuilder::new()
                .update(RAI_HASH_PREFIX)
                .update(root.as_bytes())
                .build();
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
        {
            const ENTRY_SIZE: usize = RaiVoteMetadata::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE;
            if bytes.len() % ENTRY_SIZE != 0 || bytes.len() / ENTRY_SIZE > Self::MAX_HASHES {
                return Err(DeserializationError::InvalidData);
            }
            let count = bytes.len() / ENTRY_SIZE;
            let mut metadata = Vec::with_capacity(count);
            let mut hashes = Vec::with_capacity(count);
            while !bytes.is_empty() {
                metadata.push(RaiVoteMetadata::deserialize(&mut bytes)?);
                hashes.push(BlockHash::deserialize(&mut bytes)?);
            }
            let vote = Self {
                timestamp,
                metadata,
                voter,
                signature,
                hashes,
            };
            if !vote.rai_batch_is_canonical() {
                return Err(DeserializationError::InvalidData);
            }
            return Ok(vote);
        }

        #[cfg(not(feature = "rai_protocol"))]
        {
            let mut hashes = Vec::new();
            while !bytes.is_empty() && hashes.len() < Self::MAX_HASHES {
                hashes.push(BlockHash::deserialize(&mut bytes)?);
            }
            Ok(Self {
                timestamp,
                voter,
                signature,
                hashes,
            })
        }
    }

    pub fn validate(&self) -> Result<(), SignatureError> {
        #[cfg(feature = "rai_protocol")]
        if !self.rai_batch_is_canonical() {
            return Err(SignatureError {});
        }
        self.voter.verify(self.hash().as_bytes(), &self.signature)
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_batch_is_canonical(&self) -> bool {
        self.metadata.len() == self.hashes.len()
            && self.hashes.len() <= Self::MAX_HASHES
            && (1..self.hashes.len()).all(|i| {
                (&self.metadata[i - 1], &self.hashes[i - 1]) < (&self.metadata[i], &self.hashes[i])
            })
    }

    pub const fn serialized_size(count: usize) -> usize {
        let base =
            Account::SERIALIZED_SIZE + Signature::SERIALIZED_SIZE + std::mem::size_of::<u64>(); // timestamp
        #[cfg(feature = "rai_protocol")]
        return base + ((RaiVoteMetadata::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE) * count);
        #[cfg(not(feature = "rai_protocol"))]
        return base + (BlockHash::SERIALIZED_SIZE * count);
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        #[cfg(feature = "rai_protocol")]
        if !self.rai_batch_is_canonical() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "RAI vote entries must be aligned, canonical, and duplicate-free",
            ));
        }
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)?;
        writer.write_all(&self.timestamp.to_le_bytes())?;
        #[cfg(feature = "rai_protocol")]
        for (metadata, hash) in self.rai_entries() {
            metadata.serialize(writer)?;
            hash.serialize(writer)?;
        }
        #[cfg(not(feature = "rai_protocol"))]
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
            return Vote::new_rai_batch(
                &self.key,
                timestamp,
                duration,
                self.hashes
                    .into_iter()
                    .map(|hash| (self.metadata.clone(), hash)),
            );
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
    fn rai_metadata() -> RaiVoteMetadata {
        RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::new(7),
                root: QualifiedRoot::new_test_instance(),
            }),
            phase: RaiVotePhase::Notar,
            epoch: RaiEpoch::new(7),
            scope: RaiCommitteeScope::Older,
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_vote() -> Vote {
        let metadata = rai_metadata();
        Vote::new_rai_batch(
            &PrivateKey::from(42),
            UnixMillisTimestamp::new(0x12340),
            3,
            [
                (metadata.clone(), BlockHash::from(12)),
                (metadata, BlockHash::from(11)),
            ],
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
    fn rai_batch_is_sorted_deduplicated_and_exposed_as_aligned_entries() {
        let metadata = rai_metadata();
        let vote = Vote::new_rai_batch(
            &PrivateKey::from(42),
            UnixMillisTimestamp::new(0x12340),
            3,
            [
                (metadata.clone(), BlockHash::from(12)),
                (metadata.clone(), BlockHash::from(11)),
                (metadata.clone(), BlockHash::from(12)),
            ],
        );

        let entries: Vec<_> = vote
            .rai_entries()
            .map(|(metadata, hash)| (metadata.clone(), *hash))
            .collect();
        assert_eq!(
            entries,
            vec![
                (metadata.clone(), BlockHash::from(11)),
                (metadata.clone(), BlockHash::from(12)),
            ]
        );
        assert_eq!(vote.rai_entry_count(), 2);
        assert_eq!(vote.rai_metadata(0), Some(&metadata));
        assert_eq!(vote.rai_metadata(2), None);
        assert!(vote.validate().is_ok());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_singleton_constructor_creates_one_leaf() {
        let metadata = rai_metadata();
        let hash = BlockHash::from(11);
        let vote = Vote::new_rai(
            &PrivateKey::from(42),
            UnixMillisTimestamp::new(0x12340),
            3,
            hash,
            metadata.clone(),
        );

        assert_eq!(vote.rai_entries().next(), Some((&metadata, &hash)));
        assert!(vote.validate().is_ok());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_empty_batch_preserves_legacy_zero_hash_vote_compatibility() {
        let vote = Vote::new_rai_batch(
            &PrivateKey::from(42),
            UnixMillisTimestamp::new(0x12340),
            3,
            [],
        );
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), Vote::serialized_size(0));
        assert_eq!(Vote::deserialize(&bytes).unwrap(), vote);
        assert!(vote.validate().is_ok());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn generated_vote_phase_matches_legacy_final_timestamp() {
        let key = PrivateKey::from(42);
        let hash = BlockHash::from(11);

        let first = Vote::new(&key, UnixMillisTimestamp::new(16), 0, vec![hash]);
        let final_vote = Vote::new_final(&key, vec![hash]);

        assert_eq!(first.metadata[0].phase, RaiVotePhase::First);
        assert_eq!(final_vote.metadata[0].phase, RaiVotePhase::Final);
        assert_eq!(final_vote.metadata[0].epoch, RaiEpoch::ZERO);
        assert_eq!(final_vote.metadata[0].scope, RaiCommitteeScope::All);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_phase() {
        let mut vote = rai_vote();
        vote.metadata[0].phase = RaiVotePhase::Final;
        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_election_epoch() {
        let mut vote = rai_vote();
        let RaiElectionId::Slot(id) = &mut vote.metadata[0].election_id else {
            panic!("test vote must target a slot election");
        };
        id.epoch = RaiEpoch::new(id.epoch.number() + 1);

        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_epoch() {
        let mut vote = rai_vote();
        vote.metadata[0].epoch = RaiEpoch::new(8);
        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_committee_scope() {
        let mut vote = rai_vote();
        vote.metadata[0].scope = RaiCommitteeScope::Newer;
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
    fn rai_signature_binds_metadata_for_every_leaf() {
        for index in 0..2 {
            let mut vote = rai_vote();
            let old_hash = vote.hash();
            vote.metadata[index].epoch = RaiEpoch::new(100 + index as u64);
            assert_ne!(vote.hash(), old_hash);
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

        bytes[metadata_offset + 1 + RaiElectionId::SERIALIZED_SIZE + 8] = 3;
        assert!(Vote::deserialize(&bytes).is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_deserialization_rejects_partial_noncanonical_and_duplicate_entries() {
        const BASE_SIZE: usize = PublicKey::SERIALIZED_SIZE + Signature::SERIALIZED_SIZE + 8;
        const ENTRY_SIZE: usize = RaiVoteMetadata::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE;
        let vote = rai_vote();
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();

        let mut partial = bytes.clone();
        partial.push(0);
        assert!(Vote::deserialize(&partial).is_err());

        let first = bytes[BASE_SIZE..BASE_SIZE + ENTRY_SIZE].to_vec();
        let second = bytes[BASE_SIZE + ENTRY_SIZE..BASE_SIZE + 2 * ENTRY_SIZE].to_vec();

        let mut noncanonical = bytes.clone();
        noncanonical[BASE_SIZE..BASE_SIZE + ENTRY_SIZE].copy_from_slice(&second);
        noncanonical[BASE_SIZE + ENTRY_SIZE..BASE_SIZE + 2 * ENTRY_SIZE].copy_from_slice(&first);
        assert!(Vote::deserialize(&noncanonical).is_err());

        let mut duplicate = bytes;
        duplicate[BASE_SIZE + ENTRY_SIZE..BASE_SIZE + 2 * ENTRY_SIZE].copy_from_slice(&first);
        assert!(Vote::deserialize(&duplicate).is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_serialize_and_validate_reject_misaligned_entries() {
        let mut vote = rai_vote();
        vote.metadata.pop();

        assert!(vote.serialize(&mut Vec::new()).is_err());
        assert!(vote.validate().is_err());
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
