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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RaiTimeoutSlot {
    pub account: Account,
    pub height: u64,
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RaiVoteTarget {
    Hash(BlockHash),
    Timeout(RaiTimeoutSlot),
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RaiVoteEntry {
    pub metadata: RaiVoteMetadata,
    pub target: RaiVoteTarget,
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiBatchKind {
    Slot,
    TimeoutSlot,
    Close,
    Full,
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RaiSharedContext {
    phase: RaiVotePhase,
    epoch: RaiEpoch,
    scope: RaiCommitteeScope,
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

    pub fn epoch(&self) -> RaiEpoch {
        match self {
            Self::Slot(id) => id.epoch,
            Self::CloseCut { epoch, .. } | Self::CloseRecord { epoch, .. } => *epoch,
        }
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
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RaiVoteMetadata {
    pub election_id: RaiElectionId,
    pub phase: RaiVotePhase,
    pub epoch: RaiEpoch,
    pub scope: RaiCommitteeScope,
}

#[cfg(feature = "rai_protocol")]
impl RaiVoteMetadata {
    // The election id already carries the epoch. Keeping `epoch` in memory is
    // convenient for callers, but serializing it again added eight redundant
    // bytes to every logical vote leaf.
    pub const SERIALIZED_SIZE: usize = 1 + RaiElectionId::SERIALIZED_SIZE + 1;
    pub const SLOT_SERIALIZED_SIZE: usize = 1 + 8 + 1;
    pub const TIMEOUT_SLOT_SERIALIZED_SIZE: usize = Self::SLOT_SERIALIZED_SIZE + 32 + 8;

    /// Reserved context for a signed representative-discovery response. It is
    /// never admissible as slot or close-election evidence.
    pub fn is_discovery(&self) -> bool {
        self.election_id == RaiElectionId::default()
    }

    fn serialize_slot<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&[self.phase as u8])?;
        writer.write_all(&self.epoch.number().to_le_bytes())?;
        writer.write_all(&[self.scope as u8])
    }

    fn deserialize_slot<T: Read>(reader: &mut T) -> Result<Self, DeserializationError> {
        let phase = match crate::read_u8(reader)? {
            0 => RaiVotePhase::First,
            1 => RaiVotePhase::Notar,
            2 => RaiVotePhase::Final,
            _ => return Err(DeserializationError::InvalidData),
        };
        let mut epoch_bytes = [0; 8];
        reader.read_exact(&mut epoch_bytes)?;
        let epoch = RaiEpoch::new(u64::from_le_bytes(epoch_bytes));
        let scope = match crate::read_u8(reader)? {
            0 => RaiCommitteeScope::All,
            1 => RaiCommitteeScope::Older,
            2 => RaiCommitteeScope::Newer,
            _ => return Err(DeserializationError::InvalidData),
        };
        Ok(Self {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch,
                root: QualifiedRoot::ZERO,
            }),
            phase,
            epoch,
            scope,
        })
    }

    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&[self.phase as u8])?;
        self.election_id.serialize(writer)?;
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
        let epoch = election_id.epoch();
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
    entries: Vec<RaiVoteEntry>,
    /// True only when this transport was decoded from the compact-slot wire
    /// representation. This disambiguates its omitted root from the legacy
    /// contextless discovery sentinel, which also uses a zero root.
    #[cfg(feature = "rai_protocol")]
    compact_rai_slot: bool,
    #[cfg(feature = "rai_protocol")]
    batch_kind: Option<RaiBatchKind>,
    #[cfg(feature = "rai_protocol")]
    shared_context: Option<RaiSharedContext>,

    // Account that's voting
    pub voter: PublicKey,

    // Signature of the vote digest (including all RAI metadata/hash leaves)
    pub signature: Signature,

    // The hashes for which this vote directly covers
    #[cfg(not(feature = "rai_protocol"))]
    pub hashes: Vec<BlockHash>,
}

#[cfg(not(feature = "rai_protocol"))]
static HASH_PREFIX: &str = "vote ";
#[cfg(feature = "rai_protocol")]
static RAI_HASH_PREFIX: &[u8] = b"RAI/VoteBatch/v3";

impl Vote {
    pub const MAX_HASHES: usize = 255;

    pub fn len(&self) -> usize {
        #[cfg(feature = "rai_protocol")]
        {
            self.entries.len()
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            self.hashes.len()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[cfg(feature = "rai_protocol")]
    const RAI_SHARED_WIRE_VERSION: u8 = 1;
    #[cfg(feature = "rai_protocol")]
    const RAI_SHARED_HEADER_SIZE: usize = 1 + 1 + 1 + 8 + 1;

    #[cfg(feature = "rai_protocol")]
    fn classify_entries(entries: &[RaiVoteEntry]) -> Option<RaiBatchKind> {
        if entries.is_empty() {
            return None;
        }
        let all_slots = entries.iter().all(|entry| {
            entry.metadata.epoch == entry.metadata.election_id.epoch()
                && matches!(entry.metadata.election_id, RaiElectionId::Slot(_))
        });
        let all_close = entries.iter().all(|entry| {
            entry.metadata.epoch == entry.metadata.election_id.epoch()
                && matches!(
                    entry.metadata.election_id,
                    RaiElectionId::CloseCut { .. } | RaiElectionId::CloseRecord { .. }
                )
                && matches!(entry.target, RaiVoteTarget::Hash(_))
        });
        if all_close {
            Some(RaiBatchKind::Close)
        } else if all_slots
            && entries
                .iter()
                .all(|entry| matches!(entry.target, RaiVoteTarget::Hash(hash) if !hash.is_zero()))
        {
            Some(RaiBatchKind::Slot)
        } else if all_slots
            && entries
                .iter()
                .all(|entry| matches!(entry.target, RaiVoteTarget::Hash(_)))
        {
            Some(RaiBatchKind::Full)
        } else if all_slots
            && entries.iter().all(
                |entry| matches!(entry.target, RaiVoteTarget::Timeout(slot) if slot.height != 0),
            )
        {
            Some(RaiBatchKind::TimeoutSlot)
        } else {
            None
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn compare_entries(
        kind: RaiBatchKind,
        left: &RaiVoteEntry,
        right: &RaiVoteEntry,
    ) -> std::cmp::Ordering {
        let metadata_order = if matches!(kind, RaiBatchKind::Slot | RaiBatchKind::TimeoutSlot) {
            (
                left.metadata.phase,
                left.metadata.epoch,
                left.metadata.scope,
            )
                .cmp(&(
                    right.metadata.phase,
                    right.metadata.epoch,
                    right.metadata.scope,
                ))
        } else {
            left.metadata.cmp(&right.metadata)
        };
        metadata_order.then_with(|| left.target.cmp(&right.target))
    }

    #[cfg(feature = "rai_protocol")]
    fn shared_context(entries: &[RaiVoteEntry]) -> Option<RaiSharedContext> {
        let first = entries.first()?;
        let context = RaiSharedContext {
            phase: first.metadata.phase,
            epoch: first.metadata.epoch,
            scope: first.metadata.scope,
        };
        entries
            .iter()
            .all(|entry| {
                entry.metadata.phase == context.phase
                    && entry.metadata.epoch == context.epoch
                    && entry.metadata.scope == context.scope
            })
            .then_some(context)
    }

    #[cfg(feature = "rai_protocol")]
    fn metadata_digest_bytes(metadata: &RaiVoteMetadata, compact: bool) -> ([u8; 79], usize) {
        let mut bytes = [0; 79];
        let size = if compact {
            metadata
                .serialize_slot(&mut &mut bytes[..RaiVoteMetadata::SLOT_SERIALIZED_SIZE])
                .expect("fixed metadata buffer");
            RaiVoteMetadata::SLOT_SERIALIZED_SIZE
        } else {
            metadata
                .serialize(&mut &mut bytes[..RaiVoteMetadata::SERIALIZED_SIZE])
                .expect("fixed metadata buffer");
            RaiVoteMetadata::SERIALIZED_SIZE
        };
        (bytes, size)
    }
    pub fn null() -> Self {
        Self {
            timestamp: 0.into(),
            #[cfg(feature = "rai_protocol")]
            entries: Vec::new(),
            #[cfg(feature = "rai_protocol")]
            compact_rai_slot: false,
            #[cfg(feature = "rai_protocol")]
            batch_kind: None,
            #[cfg(feature = "rai_protocol")]
            shared_context: None,
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            #[cfg(not(feature = "rai_protocol"))]
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
        let mut entries = entries
            .into_iter()
            .map(|(metadata, hash)| RaiVoteEntry {
                metadata,
                target: RaiVoteTarget::Hash(hash),
            })
            .collect::<Vec<_>>();
        let kind = Self::classify_entries(&entries);
        if let Some(kind) = kind {
            entries.sort_unstable_by(|left, right| Self::compare_entries(kind, left, right));
            entries.dedup_by(|right, left| Self::compare_entries(kind, left, right).is_eq());
        } else {
            assert!(entries.is_empty(), "mixed or invalid RAI vote batch");
        }
        assert!(entries.len() <= Self::MAX_HASHES);
        let shared_context = Self::shared_context(&entries);
        let mut result = Self {
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            entries,
            compact_rai_slot: false,
            batch_kind: kind,
            shared_context,
        };
        result.signature = priv_key.sign(result.hash().as_bytes());
        result
    }

    #[cfg(feature = "rai_protocol")]
    /// Signs entries which the caller already placed in canonical order.
    /// This is intended for generators that can maintain ordering while
    /// assembling a batch and avoids an O(n log n) sort on the signing path.
    pub fn new_canonical_rai_batch(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        entries: impl IntoIterator<Item = (RaiVoteMetadata, BlockHash)>,
    ) -> Self {
        let entries = entries
            .into_iter()
            .map(|(metadata, hash)| RaiVoteEntry {
                metadata,
                target: RaiVoteTarget::Hash(hash),
            })
            .collect::<Vec<_>>();
        let kind = Self::classify_entries(&entries).expect("mixed or invalid RAI vote batch");
        assert!(entries.len() <= Self::MAX_HASHES);
        assert!(
            (1..entries.len())
                .all(|i| Self::compare_entries(kind, &entries[i - 1], &entries[i]).is_lt())
        );
        let shared_context = Self::shared_context(&entries);
        let mut result = Self {
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            entries,
            compact_rai_slot: false,
            batch_kind: Some(kind),
            shared_context,
        };
        result.signature = priv_key.sign(result.hash().as_bytes());
        result
    }

    #[cfg(feature = "rai_protocol")]
    /// Iterates over the aligned logical leaves covered by this signature.
    pub fn rai_entries(
        &self,
    ) -> impl ExactSizeIterator<Item = (&RaiVoteMetadata, &BlockHash)> + '_ {
        self.entries.iter().map(|entry| {
            let hash = match &entry.target {
                RaiVoteTarget::Hash(hash) => hash,
                RaiVoteTarget::Timeout(_) => &BlockHash::ZERO,
            };
            (&entry.metadata, hash)
        })
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_entries_mut(
        &mut self,
    ) -> impl ExactSizeIterator<Item = (&mut RaiVoteMetadata, &mut RaiVoteTarget)> + '_ {
        self.entries
            .iter_mut()
            .map(|entry| (&mut entry.metadata, &mut entry.target))
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_metadata_iter(&self) -> impl ExactSizeIterator<Item = &RaiVoteMetadata> + '_ {
        self.entries.iter().map(|entry| &entry.metadata)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_metadata(&self, index: usize) -> Option<&RaiVoteMetadata> {
        self.entries.get(index).map(|entry| &entry.metadata)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_timeout_slot(&self, index: usize) -> Option<RaiTimeoutSlot> {
        self.entries
            .get(index)
            .and_then(|entry| match entry.target {
                RaiVoteTarget::Timeout(slot) => Some(slot),
                RaiVoteTarget::Hash(_) => None,
            })
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_rai_timeout_batch(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        entries: impl IntoIterator<Item = (RaiVoteMetadata, RaiTimeoutSlot)>,
    ) -> Self {
        let mut entries = entries
            .into_iter()
            .map(|(metadata, slot)| RaiVoteEntry {
                metadata,
                target: RaiVoteTarget::Timeout(slot),
            })
            .collect::<Vec<_>>();
        let kind = Self::classify_entries(&entries).expect("invalid RAI timeout batch");
        entries.sort_unstable_by(|left, right| Self::compare_entries(kind, left, right));
        entries.dedup_by(|right, left| Self::compare_entries(kind, left, right).is_eq());
        assert!(entries.len() <= Self::MAX_HASHES);
        let shared_context = Self::shared_context(&entries);
        let mut result = Self {
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            entries,
            compact_rai_slot: false,
            batch_kind: Some(kind),
            shared_context,
        };
        result.signature = priv_key.sign(result.hash().as_bytes());
        result
    }

    #[cfg(feature = "rai_protocol")]
    pub fn new_canonical_rai_timeout_batch(
        priv_key: &PrivateKey,
        timestamp: UnixMillisTimestamp,
        duration: u8,
        entries: impl IntoIterator<Item = (RaiVoteMetadata, RaiTimeoutSlot)>,
    ) -> Self {
        let entries = entries
            .into_iter()
            .map(|(metadata, slot)| RaiVoteEntry {
                metadata,
                target: RaiVoteTarget::Timeout(slot),
            })
            .collect::<Vec<_>>();
        let kind = Self::classify_entries(&entries).expect("invalid RAI timeout batch");
        assert!(entries.len() <= Self::MAX_HASHES);
        assert!(
            (1..entries.len())
                .all(|i| Self::compare_entries(kind, &entries[i - 1], &entries[i]).is_lt())
        );
        let shared_context = Self::shared_context(&entries);
        let mut result = Self {
            voter: priv_key.public_key(),
            timestamp: VoteTimestamp::new(timestamp, duration),
            signature: Signature::new(),
            entries,
            compact_rai_slot: false,
            batch_kind: Some(kind),
            shared_context,
        };
        result.signature = priv_key.sign(result.hash().as_bytes());
        result
    }

    #[cfg(feature = "rai_protocol")]
    pub fn is_compact_rai_slot(&self) -> bool {
        self.compact_rai_slot
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_entry_count(&self) -> usize {
        self.entries.len()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn hashes(&self) -> impl ExactSizeIterator<Item = &BlockHash> {
        self.entries.iter().map(|entry| match &entry.target {
            RaiVoteTarget::Hash(hash) => hash,
            RaiVoteTarget::Timeout(_) => &BlockHash::ZERO,
        })
    }

    #[cfg(not(feature = "rai_protocol"))]
    pub fn hashes(&self) -> impl ExactSizeIterator<Item = &BlockHash> {
        self.hashes.iter()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn is_rai_slot_batch(&self) -> bool {
        self.batch_kind == Some(RaiBatchKind::Slot)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn is_rai_timeout_slot_batch(&self) -> bool {
        self.batch_kind == Some(RaiBatchKind::TimeoutSlot)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn uses_rai_shared_context(&self) -> bool {
        self.shared_context.is_some() && self.batch_kind == Some(RaiBatchKind::Slot)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn rai_batch_kind(&self) -> Option<RaiBatchKind> {
        self.batch_kind
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
                .update([self.entries.len() as u8]);
            let compact = matches!(
                self.batch_kind,
                Some(RaiBatchKind::Slot | RaiBatchKind::TimeoutSlot)
            );
            for entry in &self.entries {
                let (bytes, size) = Self::metadata_digest_bytes(&entry.metadata, compact);
                root_builder = root_builder.update(&bytes[..size]);
                match &entry.target {
                    RaiVoteTarget::Timeout(timeout) => {
                        root_builder = root_builder
                            .update(timeout.account.as_bytes())
                            .update(timeout.height.to_le_bytes());
                    }
                    RaiVoteTarget::Hash(hash) => {
                        root_builder = root_builder.update(hash.as_bytes());
                    }
                }
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

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DeserializationError> {
        #[cfg(feature = "rai_protocol")]
        {
            let base = Account::SERIALIZED_SIZE + Signature::SERIALIZED_SIZE + 8;
            let payload = bytes.len().saturating_sub(base);
            let slot_entry = RaiVoteMetadata::SLOT_SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE;
            let full_entry = RaiVoteMetadata::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE;
            let compact_slot =
                payload != 0 && payload % slot_entry == 0 && payload % full_entry != 0;
            return Self::deserialize_with_rai_slot(bytes, compact_slot);
        }
        #[cfg(not(feature = "rai_protocol"))]
        Self::deserialize_with_rai_slot(bytes, false)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn deserialize_rai_slot(bytes: &[u8]) -> Result<Self, DeserializationError> {
        Self::deserialize_with_rai_slot(bytes, true)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn deserialize_rai_timeout_slot(bytes: &[u8]) -> Result<Self, DeserializationError> {
        Self::deserialize_with_rai_mode(bytes, 2)
    }

    #[cfg(feature = "rai_protocol")]
    pub fn deserialize_rai_shared(
        mut bytes: &[u8],
        expected_kind: RaiBatchKind,
    ) -> Result<Self, DeserializationError> {
        let voter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut timestamp_bytes = [0; 8];
        bytes.read_exact(&mut timestamp_bytes)?;
        let timestamp = VoteTimestamp::from_le_bytes(timestamp_bytes);
        if crate::read_u8(&mut bytes)? != Self::RAI_SHARED_WIRE_VERSION {
            return Err(DeserializationError::InvalidData);
        }
        let kind = match crate::read_u8(&mut bytes)? {
            0 => RaiBatchKind::Slot,
            1 => RaiBatchKind::TimeoutSlot,
            2 => RaiBatchKind::Close,
            _ => return Err(DeserializationError::InvalidData),
        };
        if kind != expected_kind {
            return Err(DeserializationError::InvalidData);
        }
        let phase = match crate::read_u8(&mut bytes)? {
            0 => RaiVotePhase::First,
            1 => RaiVotePhase::Notar,
            2 => RaiVotePhase::Final,
            _ => return Err(DeserializationError::InvalidData),
        };
        let mut epoch_bytes = [0; 8];
        bytes.read_exact(&mut epoch_bytes)?;
        let epoch = RaiEpoch::new(u64::from_le_bytes(epoch_bytes));
        let scope = match crate::read_u8(&mut bytes)? {
            0 => RaiCommitteeScope::All,
            1 => RaiCommitteeScope::Older,
            2 => RaiCommitteeScope::Newer,
            _ => return Err(DeserializationError::InvalidData),
        };
        let entry_size = match kind {
            RaiBatchKind::Slot => 32,
            RaiBatchKind::TimeoutSlot => 40,
            RaiBatchKind::Close => 37,
            RaiBatchKind::Full => return Err(DeserializationError::InvalidData),
        };
        if bytes.is_empty()
            || bytes.len() % entry_size != 0
            || bytes.len() / entry_size > Self::MAX_HASHES
        {
            return Err(DeserializationError::InvalidData);
        }
        let mut entries = Vec::with_capacity(bytes.len() / entry_size);
        while !bytes.is_empty() {
            let (election_id, target) = match kind {
                RaiBatchKind::Slot => (
                    RaiElectionId::Slot(RaiSlotId {
                        epoch,
                        root: QualifiedRoot::ZERO,
                    }),
                    RaiVoteTarget::Hash(BlockHash::deserialize(&mut bytes)?),
                ),
                RaiBatchKind::TimeoutSlot => {
                    let account = Account::deserialize(&mut bytes)?;
                    let mut height = [0; 8];
                    bytes.read_exact(&mut height)?;
                    let height = u64::from_le_bytes(height);
                    if height == 0 {
                        return Err(DeserializationError::InvalidData);
                    }
                    (
                        RaiElectionId::Slot(RaiSlotId {
                            epoch,
                            root: QualifiedRoot::ZERO,
                        }),
                        RaiVoteTarget::Timeout(RaiTimeoutSlot { account, height }),
                    )
                }
                RaiBatchKind::Close => {
                    let close_kind = crate::read_u8(&mut bytes)?;
                    let mut round = [0; 4];
                    bytes.read_exact(&mut round)?;
                    let round = u32::from_le_bytes(round);
                    let election_id = match close_kind {
                        0 => RaiElectionId::CloseCut { epoch, round },
                        1 => RaiElectionId::CloseRecord { epoch, round },
                        _ => return Err(DeserializationError::InvalidData),
                    };
                    (
                        election_id,
                        RaiVoteTarget::Hash(BlockHash::deserialize(&mut bytes)?),
                    )
                }
                RaiBatchKind::Full => return Err(DeserializationError::InvalidData),
            };
            entries.push(RaiVoteEntry {
                metadata: RaiVoteMetadata {
                    election_id,
                    phase,
                    epoch,
                    scope,
                },
                target,
            });
        }
        let shared_context = Self::shared_context(&entries);
        let vote = Self {
            timestamp,
            entries,
            voter,
            signature,
            compact_rai_slot: matches!(kind, RaiBatchKind::Slot),
            batch_kind: Some(kind),
            shared_context,
        };
        if !vote.rai_batch_is_canonical() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(vote)
    }

    fn deserialize_with_rai_slot(
        bytes: &[u8],
        #[cfg(feature = "rai_protocol")] compact_slot: bool,
        #[cfg(not(feature = "rai_protocol"))] _compact_slot: bool,
    ) -> Result<Self, DeserializationError> {
        #[cfg(feature = "rai_protocol")]
        let mode = if compact_slot { 1 } else { 0 };
        #[cfg(not(feature = "rai_protocol"))]
        let mode = 0;
        Self::deserialize_with_rai_mode(bytes, mode)
    }

    fn deserialize_with_rai_mode(
        mut bytes: &[u8],
        #[cfg(feature = "rai_protocol")] rai_mode: u8,
        #[cfg(not(feature = "rai_protocol"))] _rai_mode: u8,
    ) -> Result<Self, DeserializationError> {
        let voter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut buffer = [0; 8];
        bytes.read_exact(&mut buffer)?;
        let timestamp = VoteTimestamp::from_le_bytes(buffer);
        #[cfg(feature = "rai_protocol")]
        {
            let compact_slot = rai_mode == 1;
            let timeout_slot = rai_mode == 2;
            let entry_size = if compact_slot {
                RaiVoteMetadata::SLOT_SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE
            } else if timeout_slot {
                RaiVoteMetadata::TIMEOUT_SLOT_SERIALIZED_SIZE
            } else {
                RaiVoteMetadata::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE
            };
            if bytes.len() % entry_size != 0 || bytes.len() / entry_size > Self::MAX_HASHES {
                return Err(DeserializationError::InvalidData);
            }
            let count = bytes.len() / entry_size;
            let mut entries = Vec::with_capacity(count);
            while !bytes.is_empty() {
                let metadata = if compact_slot || timeout_slot {
                    RaiVoteMetadata::deserialize_slot(&mut bytes)?
                } else {
                    RaiVoteMetadata::deserialize(&mut bytes)?
                };
                let target = if timeout_slot {
                    let account = Account::deserialize(&mut bytes)?;
                    let mut height = [0; 8];
                    bytes.read_exact(&mut height)?;
                    let height = u64::from_le_bytes(height);
                    if height == 0 {
                        return Err(DeserializationError::InvalidData);
                    }
                    RaiVoteTarget::Timeout(RaiTimeoutSlot { account, height })
                } else {
                    RaiVoteTarget::Hash(BlockHash::deserialize(&mut bytes)?)
                };
                entries.push(RaiVoteEntry { metadata, target });
            }
            let batch_kind = Self::classify_entries(&entries);
            let shared_context = Self::shared_context(&entries);
            let vote = Self {
                timestamp,
                entries,
                voter,
                signature,
                compact_rai_slot: compact_slot,
                batch_kind,
                shared_context,
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
        self.entries.len() <= Self::MAX_HASHES
            && Self::classify_entries(&self.entries) == self.batch_kind
            && (1..self.entries.len()).all(|i| {
                Self::compare_entries(
                    self.batch_kind.unwrap(),
                    &self.entries[i - 1],
                    &self.entries[i],
                )
                .is_lt()
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

    #[cfg(feature = "rai_protocol")]
    pub const fn serialized_size_rai_slot(count: usize) -> usize {
        Account::SERIALIZED_SIZE
            + Signature::SERIALIZED_SIZE
            + std::mem::size_of::<u64>()
            + ((RaiVoteMetadata::SLOT_SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE) * count)
    }

    #[cfg(feature = "rai_protocol")]
    pub const fn serialized_size_rai_timeout_slot(count: usize) -> usize {
        Account::SERIALIZED_SIZE
            + Signature::SERIALIZED_SIZE
            + std::mem::size_of::<u64>()
            + (RaiVoteMetadata::TIMEOUT_SLOT_SERIALIZED_SIZE * count)
    }

    #[cfg(feature = "rai_protocol")]
    pub const fn serialized_size_rai_shared(count: usize, kind: RaiBatchKind) -> usize {
        let base = Account::SERIALIZED_SIZE
            + Signature::SERIALIZED_SIZE
            + std::mem::size_of::<u64>()
            + Self::RAI_SHARED_HEADER_SIZE;
        base + count
            * match kind {
                RaiBatchKind::Slot => 32,
                RaiBatchKind::TimeoutSlot => 40,
                RaiBatchKind::Close => 37,
                RaiBatchKind::Full => 0,
            }
    }

    #[cfg(feature = "rai_protocol")]
    pub fn serialize_rai_shared<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        if !self.rai_batch_is_canonical() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "non-canonical RAI vote batch",
            ));
        }
        let context = self.shared_context.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "RAI batch has no shared context",
            )
        })?;
        let kind = self.batch_kind.expect("validated batch kind");
        self.voter.serialize(writer)?;
        self.signature.serialize(writer)?;
        writer.write_all(&self.timestamp.to_le_bytes())?;
        writer.write_all(&[
            Self::RAI_SHARED_WIRE_VERSION,
            match kind {
                RaiBatchKind::Slot => 0,
                RaiBatchKind::TimeoutSlot => 1,
                RaiBatchKind::Close => 2,
                RaiBatchKind::Full => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "full RAI batches do not use shared context encoding",
                    ));
                }
            },
            context.phase as u8,
        ])?;
        writer.write_all(&context.epoch.number().to_le_bytes())?;
        writer.write_all(&[context.scope as u8])?;
        for entry in &self.entries {
            match (&entry.metadata.election_id, &entry.target) {
                (RaiElectionId::Slot(_), RaiVoteTarget::Hash(hash)) => hash.serialize(writer)?,
                (RaiElectionId::Slot(_), RaiVoteTarget::Timeout(slot)) => {
                    slot.account.serialize(writer)?;
                    writer.write_all(&slot.height.to_le_bytes())?;
                }
                (RaiElectionId::CloseCut { round, .. }, RaiVoteTarget::Hash(hash)) => {
                    writer.write_all(&[0])?;
                    writer.write_all(&round.to_le_bytes())?;
                    hash.serialize(writer)?;
                }
                (RaiElectionId::CloseRecord { round, .. }, RaiVoteTarget::Hash(hash)) => {
                    writer.write_all(&[1])?;
                    writer.write_all(&round.to_le_bytes())?;
                    hash.serialize(writer)?;
                }
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "RAI batch kind/target mismatch",
                    ));
                }
            }
        }
        Ok(())
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
        for entry in &self.entries {
            if matches!(
                self.batch_kind,
                Some(RaiBatchKind::Slot | RaiBatchKind::TimeoutSlot)
            ) {
                entry.metadata.serialize_slot(writer)?;
            } else {
                entry.metadata.serialize(writer)?;
            }
            match &entry.target {
                RaiVoteTarget::Timeout(timeout) => {
                    timeout.account.serialize(writer)?;
                    writer.write_all(&timeout.height.to_le_bytes())?;
                }
                RaiVoteTarget::Hash(hash) => hash.serialize(writer)?,
            }
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
                    self.entries == other.entries && self.batch_kind == other.batch_kind
                }
                #[cfg(not(feature = "rai_protocol"))]
                {
                    true
                }
            }
            && self.voter == other.voter
            && self.signature == other.signature
            && {
                #[cfg(feature = "rai_protocol")]
                {
                    true
                }
                #[cfg(not(feature = "rai_protocol"))]
                {
                    self.hashes == other.hashes
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

        assert_eq!(bytes.len(), Vote::serialized_size_rai_slot(2));
        let round_trip = Vote::deserialize(&bytes).unwrap();
        assert_eq!(round_trip.voter, vote.voter);
        assert_eq!(round_trip.signature, vote.signature);
        assert_eq!(
            round_trip.hashes().collect::<Vec<_>>(),
            vote.hashes().collect::<Vec<_>>()
        );
        assert_eq!(round_trip.entries.len(), vote.entries.len());
        for (decoded, original) in round_trip.entries.iter().zip(&vote.entries) {
            let decoded = &decoded.metadata;
            let original = &original.metadata;
            assert_eq!(decoded.phase, original.phase);
            assert_eq!(decoded.epoch, original.epoch);
            assert_eq!(decoded.scope, original.scope);
            assert!(
                matches!(decoded.election_id, RaiElectionId::Slot(ref slot) if slot.root == QualifiedRoot::ZERO)
            );
        }
        assert!(round_trip.validate().is_ok());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_timeout_vote_uses_account_height_locator() {
        let locator = RaiTimeoutSlot {
            account: Account::from(99),
            height: 123,
        };
        let vote = Vote::new_rai_timeout_batch(
            &PrivateKey::from(42),
            UnixMillisTimestamp::new(0x12340),
            3,
            [(rai_metadata(), locator)],
        );
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), Vote::serialized_size_rai_timeout_slot(1));
        let decoded = Vote::deserialize_rai_timeout_slot(&bytes).unwrap();
        assert_eq!(
            decoded.hashes().copied().collect::<Vec<_>>(),
            [BlockHash::ZERO]
        );
        assert_eq!(decoded.rai_timeout_slot(0), Some(locator));
        assert!(matches!(
            &decoded.entries[0].metadata.election_id,
            RaiElectionId::Slot(slot) if slot.root == QualifiedRoot::ZERO
        ));
        decoded.validate().unwrap();
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_close_timeout_vote_accepts_zero_hash() {
        let epoch = RaiEpoch::new(7);
        let metadata = RaiVoteMetadata {
            election_id: RaiElectionId::CloseCut { epoch, round: 2 },
            phase: RaiVotePhase::Notar,
            epoch,
            scope: RaiCommitteeScope::All,
        };
        let vote = Vote::new_canonical_rai_batch(
            &PrivateKey::from(42),
            UnixMillisTimestamp::new(0x12340),
            3,
            [(metadata.clone(), BlockHash::ZERO)],
        );

        assert_eq!(vote.rai_batch_kind(), Some(RaiBatchKind::Close));
        assert_eq!(vote.rai_metadata(0), Some(&metadata));
        assert_eq!(
            vote.hashes().copied().collect::<Vec<_>>(),
            [BlockHash::ZERO]
        );
        vote.validate().unwrap();
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

        assert_eq!(first.entries[0].metadata.phase, RaiVotePhase::First);
        assert_eq!(final_vote.entries[0].metadata.phase, RaiVotePhase::Final);
        assert_eq!(final_vote.entries[0].metadata.epoch, RaiEpoch::ZERO);
        assert_eq!(final_vote.entries[0].metadata.scope, RaiCommitteeScope::All);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_phase() {
        let mut vote = rai_vote();
        vote.entries[0].metadata.phase = RaiVotePhase::Final;
        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_election_epoch() {
        let mut vote = rai_vote();
        let RaiElectionId::Slot(id) = &mut vote.entries[0].metadata.election_id else {
            panic!("test vote must target a slot election");
        };
        id.epoch = RaiEpoch::new(id.epoch.number() + 1);

        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_epoch() {
        let mut vote = rai_vote();
        vote.entries[0].metadata.epoch = RaiEpoch::new(8);
        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_committee_scope() {
        let mut vote = rai_vote();
        vote.entries[0].metadata.scope = RaiCommitteeScope::Newer;
        assert!(vote.validate().is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_signature_binds_every_candidate_hash() {
        for index in 0..2 {
            let mut vote = rai_vote();
            vote.entries[index].target = RaiVoteTarget::Hash(BlockHash::from(100 + index as u64));
            assert!(vote.validate().is_err());
        }
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_rejects_metadata_epoch_inconsistent_with_election_id() {
        for index in 0..2 {
            let mut vote = rai_vote();
            let old_hash = vote.hash();
            vote.entries[index].metadata.epoch = RaiEpoch::new(100 + index as u64);
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

        bytes[metadata_offset + 1 + 8] = 3;
        assert!(Vote::deserialize(&bytes).is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_deserialization_rejects_partial_noncanonical_and_duplicate_entries() {
        const BASE_SIZE: usize = PublicKey::SERIALIZED_SIZE + Signature::SERIALIZED_SIZE + 8;
        let vote = rai_vote();
        let entry_size = if vote.is_rai_slot_batch() {
            RaiVoteMetadata::SLOT_SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE
        } else {
            RaiVoteMetadata::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE
        };
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();

        let mut partial = bytes.clone();
        partial.push(0);
        assert!(Vote::deserialize(&partial).is_err());

        let first = bytes[BASE_SIZE..BASE_SIZE + entry_size].to_vec();
        let second = bytes[BASE_SIZE + entry_size..BASE_SIZE + 2 * entry_size].to_vec();

        let mut noncanonical = bytes.clone();
        noncanonical[BASE_SIZE..BASE_SIZE + entry_size].copy_from_slice(&second);
        noncanonical[BASE_SIZE + entry_size..BASE_SIZE + 2 * entry_size].copy_from_slice(&first);
        assert!(Vote::deserialize(&noncanonical).is_err());

        let mut duplicate = bytes;
        duplicate[BASE_SIZE + entry_size..BASE_SIZE + 2 * entry_size].copy_from_slice(&first);
        assert!(Vote::deserialize(&duplicate).is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_serialize_and_validate_reject_inconsistent_cached_kind() {
        let mut vote = rai_vote();
        vote.batch_kind = Some(RaiBatchKind::Close);

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
        for hash in vote.hashes() {
            hash.serialize(&mut expected).unwrap();
        }

        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 32 + 64 + 8 + 2 * 32);
    }
}
