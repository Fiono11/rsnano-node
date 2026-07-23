use std::{
    collections::BTreeMap,
    io::{Read, Write},
};

use num_traits::FromPrimitive;

use crate::{
    Account, Blake2HashBuilder, BlockHash, DeserializationError, PrivateKey, ProtocolInfo,
    PublicKey, Signature, SignatureError, read_u8, read_u64_be,
};

pub const RAI_PROTOCOL_VERSION: u8 = ProtocolInfo::RAI_PROTOCOL_VERSION;

pub const RAI_ELECTION_KIND_SLOT: u8 = 0;
pub const RAI_ELECTION_KIND_CLOSE_CUT: u8 = 1;
pub const RAI_ELECTION_KIND_CLOSE_RECORD: u8 = 2;

pub const RAI_VOTE_KIND_FIRST: u8 = 0;
pub const RAI_VOTE_KIND_NOTARIZATION: u8 = 1;
pub const RAI_VOTE_KIND_FINAL: u8 = 2;

pub const RAI_VOTE_SCOPE_ALL: u8 = 0;
pub const RAI_VOTE_SCOPE_COMMITTEE: u8 = 1;

pub const RAI_ELECTION_VALUE_KIND_BLOCK: u8 = 0;
pub const RAI_ELECTION_VALUE_KIND_CLOSE_CUT_HASH: u8 = 1;
pub const RAI_ELECTION_VALUE_KIND_CLOSE_RECORD_HASH: u8 = 2;
pub const RAI_ELECTION_VALUE_KIND_TIMEOUT: u8 = 3;

pub const RAI_PENDING_REPORT_MAX_PAYLOAD_SIZE: usize = 256 * 1024;
pub const RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES: u16 = 512;

const RAI_VOTE_HASH_PREFIX: &[u8] = b"rai vote ";
const RAI_CLOSE_RECORD_HASH_PREFIX: &[u8] = b"RAI/CloseRecord";
const RAI_PENDING_REPORT_HASH_PREFIX: &[u8] = b"rai pending report ";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct RaiSlot {
    pub account: Account,
    pub account_height: u64,
}

impl RaiSlot {
    pub const SERIALIZED_SIZE: usize = Account::SERIALIZED_SIZE + std::mem::size_of::<u64>();

    pub fn new(account: Account, account_height: u64) -> Self {
        Self {
            account,
            account_height,
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.account.serialize(writer)?;
        writer.write_all(&self.account_height.to_be_bytes())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let account = Account::deserialize(reader)?;
        let account_height = read_u64_be(reader)?;
        Ok(Self::new(account, account_height))
    }
}

pub type RaiEpoch = u64;
pub type RaiCloseAttempt = u64;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RaiElectionId {
    Slot {
        slot: RaiSlot,
        epoch: RaiEpoch,
    },
    CloseCut {
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    },
    CloseRecord {
        epoch: RaiEpoch,
        attempt: RaiCloseAttempt,
    },
}

impl RaiElectionId {
    pub const SERIALIZED_SIZE: usize =
        std::mem::size_of::<u8>() + RaiSlot::SERIALIZED_SIZE + std::mem::size_of::<u64>() * 2;

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match self {
            Self::Slot { slot, epoch } => {
                writer.write_all(&[RaiElectionKind::Slot as u8])?;
                slot.serialize(writer)?;
                writer.write_all(&epoch.to_be_bytes())?;
                writer.write_all(&0u64.to_be_bytes())
            }
            Self::CloseCut { epoch, attempt } => {
                writer.write_all(&[RaiElectionKind::CloseCut as u8])?;
                RaiSlot::default().serialize(writer)?;
                writer.write_all(&epoch.to_be_bytes())?;
                writer.write_all(&attempt.to_be_bytes())
            }
            Self::CloseRecord { epoch, attempt } => {
                writer.write_all(&[RaiElectionKind::CloseRecord as u8])?;
                RaiSlot::default().serialize(writer)?;
                writer.write_all(&epoch.to_be_bytes())?;
                writer.write_all(&attempt.to_be_bytes())
            }
        }
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let kind =
            RaiElectionKind::from_u8(read_u8(reader)?).ok_or(DeserializationError::InvalidData)?;
        let slot = RaiSlot::deserialize(reader)?;
        let epoch = read_u64_be(reader)?;
        let attempt = read_u64_be(reader)?;

        match kind {
            RaiElectionKind::Slot => Ok(Self::Slot { slot, epoch }),
            RaiElectionKind::CloseCut => Ok(Self::CloseCut { epoch, attempt }),
            RaiElectionKind::CloseRecord => Ok(Self::CloseRecord { epoch, attempt }),
        }
    }
}

#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum RaiElectionKind {
    Slot = RAI_ELECTION_KIND_SLOT,
    CloseCut = RAI_ELECTION_KIND_CLOSE_CUT,
    CloseRecord = RAI_ELECTION_KIND_CLOSE_RECORD,
}

#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum RaiVoteKind {
    First = RAI_VOTE_KIND_FIRST,
    Notarization = RAI_VOTE_KIND_NOTARIZATION,
    Final = RAI_VOTE_KIND_FINAL,
}

impl RaiVoteKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Notarization => "notarization",
            Self::Final => "final",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RaiVoteScope {
    All,
    Committee(u8),
}

impl RaiVoteScope {
    pub const SERIALIZED_SIZE: usize = std::mem::size_of::<u8>() * 2;

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match self {
            Self::All => writer.write_all(&[RaiVoteScopeKind::All as u8, 0]),
            Self::Committee(index) => {
                writer.write_all(&[RaiVoteScopeKind::Committee as u8, *index])
            }
        }
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let kind =
            RaiVoteScopeKind::from_u8(read_u8(reader)?).ok_or(DeserializationError::InvalidData)?;
        let committee_index = read_u8(reader)?;

        match kind {
            RaiVoteScopeKind::All if committee_index == 0 => Ok(Self::All),
            RaiVoteScopeKind::All => Err(DeserializationError::InvalidData),
            RaiVoteScopeKind::Committee => Ok(Self::Committee(committee_index)),
        }
    }
}

#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum RaiVoteScopeKind {
    All = RAI_VOTE_SCOPE_ALL,
    Committee = RAI_VOTE_SCOPE_COMMITTEE,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RaiElectionValue {
    Block(BlockHash),
    CloseCutHash(BlockHash),
    CloseRecordHash(BlockHash),
    Timeout,
}

impl RaiElectionValue {
    pub const SERIALIZED_SIZE: usize = std::mem::size_of::<u8>() + BlockHash::SERIALIZED_SIZE;

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match self {
            Self::Block(hash) => {
                writer.write_all(&[RaiElectionValueKind::Block as u8])?;
                hash.serialize(writer)
            }
            Self::CloseCutHash(hash) => {
                writer.write_all(&[RaiElectionValueKind::CloseCutHash as u8])?;
                hash.serialize(writer)
            }
            Self::CloseRecordHash(hash) => {
                writer.write_all(&[RaiElectionValueKind::CloseRecordHash as u8])?;
                hash.serialize(writer)
            }
            Self::Timeout => {
                writer.write_all(&[RaiElectionValueKind::Timeout as u8])?;
                BlockHash::ZERO.serialize(writer)
            }
        }
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let kind = RaiElectionValueKind::from_u8(read_u8(reader)?)
            .ok_or(DeserializationError::InvalidData)?;
        let hash = BlockHash::deserialize(reader)?;

        match kind {
            RaiElectionValueKind::Block => Ok(Self::Block(hash)),
            RaiElectionValueKind::CloseCutHash => Ok(Self::CloseCutHash(hash)),
            RaiElectionValueKind::CloseRecordHash => Ok(Self::CloseRecordHash(hash)),
            RaiElectionValueKind::Timeout => Ok(Self::Timeout),
        }
    }
}

#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum RaiElectionValueKind {
    Block = RAI_ELECTION_VALUE_KIND_BLOCK,
    CloseCutHash = RAI_ELECTION_VALUE_KIND_CLOSE_CUT_HASH,
    CloseRecordHash = RAI_ELECTION_VALUE_KIND_CLOSE_RECORD_HASH,
    Timeout = RAI_ELECTION_VALUE_KIND_TIMEOUT,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RaiCloseRecord {
    pub epoch: RaiEpoch,
    pub previous_close_hash: BlockHash,
    pub frontiers: BTreeMap<Account, BlockHash>,
}

impl RaiCloseRecord {
    pub fn new(
        epoch: RaiEpoch,
        previous_close_hash: BlockHash,
        frontiers: BTreeMap<Account, BlockHash>,
    ) -> Self {
        Self {
            epoch,
            previous_close_hash,
            frontiers,
        }
    }

    pub fn hash(&self) -> BlockHash {
        let mut bytes = Vec::with_capacity(self.serialized_size());
        self.serialize(&mut bytes)
            .expect("writing to Vec should succeed");

        Blake2HashBuilder::new()
            .update(RAI_CLOSE_RECORD_HASH_PREFIX)
            .update(bytes)
            .build()
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.previous_close_hash.serialize(writer)?;
        writer.write_all(&(self.frontiers.len() as u64).to_be_bytes())?;
        for (account, frontier) in &self.frontiers {
            account.serialize(writer)?;
            frontier.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let epoch = read_u64_be(reader)?;
        let previous_close_hash = BlockHash::deserialize(reader)?;
        let count = read_u64_be(reader)?;
        let count = usize::try_from(count).map_err(|_| DeserializationError::InvalidData)?;
        let mut frontiers = BTreeMap::new();
        for _ in 0..count {
            let account = Account::deserialize(reader)?;
            let frontier = BlockHash::deserialize(reader)?;
            if frontier.is_zero() || frontiers.insert(account, frontier).is_some() {
                return Err(DeserializationError::InvalidData);
            }
        }
        Ok(Self {
            epoch,
            previous_close_hash,
            frontiers,
        })
    }

    pub fn serialized_size(&self) -> usize {
        std::mem::size_of::<u64>()
            + BlockHash::SERIALIZED_SIZE
            + std::mem::size_of::<u64>()
            + self.frontiers.len() * (Account::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RaiVote {
    pub voter: PublicKey,
    pub kind: RaiVoteKind,
    pub scope: RaiVoteScope,
    pub election_id: RaiElectionId,
    pub value: RaiElectionValue,
    pub signature: Signature,
}

impl RaiVote {
    pub const SERIALIZED_SIZE: usize = PublicKey::SERIALIZED_SIZE
        + std::mem::size_of::<u8>()
        + RaiVoteScope::SERIALIZED_SIZE
        + RaiElectionId::SERIALIZED_SIZE
        + RaiElectionValue::SERIALIZED_SIZE
        + Signature::SERIALIZED_SIZE;

    pub fn new_first(
        key: &PrivateKey,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        Self::new_signed(
            key,
            RaiVoteKind::First,
            RaiVoteScope::All,
            election_id,
            value,
        )
    }

    pub fn new_notarization(
        key: &PrivateKey,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        Self::new_signed(
            key,
            RaiVoteKind::Notarization,
            RaiVoteScope::All,
            election_id,
            value,
        )
    }

    pub fn new_final(
        key: &PrivateKey,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        Self::new_signed(
            key,
            RaiVoteKind::Final,
            RaiVoteScope::All,
            election_id,
            value,
        )
    }

    pub fn new_first_scoped(
        key: &PrivateKey,
        committee_index: u8,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        Self::new_signed(
            key,
            RaiVoteKind::First,
            RaiVoteScope::Committee(committee_index),
            election_id,
            value,
        )
    }

    pub fn new_notarization_scoped(
        key: &PrivateKey,
        committee_index: u8,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        Self::new_signed(
            key,
            RaiVoteKind::Notarization,
            RaiVoteScope::Committee(committee_index),
            election_id,
            value,
        )
    }

    pub fn new_final_scoped(
        key: &PrivateKey,
        committee_index: u8,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        Self::new_signed(
            key,
            RaiVoteKind::Final,
            RaiVoteScope::Committee(committee_index),
            election_id,
            value,
        )
    }

    fn new_signed(
        key: &PrivateKey,
        kind: RaiVoteKind,
        scope: RaiVoteScope,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        let signature = key.sign(Self::hash_for(kind, scope, &election_id, &value).as_bytes());

        Self {
            voter: key.public_key(),
            kind,
            scope,
            election_id,
            value,
            signature,
        }
    }

    pub fn hash(&self) -> BlockHash {
        Self::hash_for(self.kind, self.scope, &self.election_id, &self.value)
    }

    pub fn hash_for(
        kind: RaiVoteKind,
        scope: RaiVoteScope,
        election_id: &RaiElectionId,
        value: &RaiElectionValue,
    ) -> BlockHash {
        let mut bytes = Vec::with_capacity(
            std::mem::size_of::<u8>()
                + RaiVoteScope::SERIALIZED_SIZE
                + RaiElectionId::SERIALIZED_SIZE
                + RaiElectionValue::SERIALIZED_SIZE,
        );
        bytes.push(kind as u8);
        scope
            .serialize(&mut bytes)
            .expect("writing to Vec should succeed");
        election_id
            .serialize(&mut bytes)
            .expect("writing to Vec should succeed");
        value
            .serialize(&mut bytes)
            .expect("writing to Vec should succeed");

        Blake2HashBuilder::new()
            .update(RAI_VOTE_HASH_PREFIX)
            .update(bytes)
            .build()
    }

    pub fn validate(&self) -> Result<(), SignatureError> {
        self.voter.verify(self.hash().as_bytes(), &self.signature)
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.voter.serialize(writer)?;
        writer.write_all(&[self.kind as u8])?;
        self.scope.serialize(writer)?;
        self.election_id.serialize(writer)?;
        self.value.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let voter = PublicKey::deserialize(&mut bytes)?;
        let kind =
            RaiVoteKind::from_u8(read_u8(&mut bytes)?).ok_or(DeserializationError::InvalidData)?;
        let scope = RaiVoteScope::deserialize(&mut bytes)?;
        let election_id = RaiElectionId::deserialize(&mut bytes)?;
        let value = RaiElectionValue::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        if !bytes.is_empty() {
            return Err(DeserializationError::TooMuchData);
        }

        Ok(Self {
            voter,
            kind,
            scope,
            election_id,
            value,
            signature,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RaiPendingReport {
    pub reporter: PublicKey,
    pub epoch: RaiEpoch,
    pub slots: Vec<RaiSlot>,
    pub signature: Signature,
}

impl RaiPendingReport {
    pub const HEADER_SIZE: usize =
        PublicKey::SERIALIZED_SIZE + std::mem::size_of::<u64>() + Signature::SERIALIZED_SIZE;
    pub const MAX_PAYLOAD_SIZE: usize = RAI_PENDING_REPORT_MAX_PAYLOAD_SIZE;
    pub const MAX_SLOTS: usize =
        (Self::MAX_PAYLOAD_SIZE - Self::HEADER_SIZE) / RaiSlot::SERIALIZED_SIZE;

    pub fn new(key: &PrivateKey, epoch: RaiEpoch, slots: Vec<RaiSlot>) -> Self {
        assert!(slots.len() <= Self::MAX_SLOTS);
        let signature = key.sign(Self::hash_for(epoch, &slots).as_bytes());

        Self {
            reporter: key.public_key(),
            epoch,
            slots,
            signature,
        }
    }

    pub fn hash(&self) -> BlockHash {
        Self::hash_for(self.epoch, &self.slots)
    }

    pub fn hash_for(epoch: RaiEpoch, slots: &[RaiSlot]) -> BlockHash {
        let mut bytes = Vec::with_capacity(
            std::mem::size_of::<u64>()
                + std::mem::size_of::<u16>()
                + slots.len() * RaiSlot::SERIALIZED_SIZE,
        );
        bytes.extend(epoch.to_be_bytes());
        bytes.extend((slots.len() as u16).to_be_bytes());
        for slot in slots {
            slot.serialize(&mut bytes)
                .expect("writing to Vec should succeed");
        }

        Blake2HashBuilder::new()
            .update(RAI_PENDING_REPORT_HASH_PREFIX)
            .update(bytes)
            .build()
    }

    pub fn validate(&self) -> Result<(), SignatureError> {
        self.reporter
            .verify(self.hash().as_bytes(), &self.signature)
    }

    pub fn serialized_size(count: usize) -> usize {
        Self::HEADER_SIZE + count * RaiSlot::SERIALIZED_SIZE
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.reporter.serialize(writer)?;
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.signature.serialize(writer)?;
        for slot in &self.slots {
            slot.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize(mut bytes: &[u8], count: usize) -> Result<Self, DeserializationError> {
        if count > Self::MAX_SLOTS {
            return Err(DeserializationError::InvalidData);
        }

        let reporter = PublicKey::deserialize(&mut bytes)?;
        let epoch = read_u64_be(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut slots = Vec::with_capacity(count);
        for _ in 0..count {
            slots.push(RaiSlot::deserialize(&mut bytes)?);
        }
        if !bytes.is_empty() {
            return Err(DeserializationError::TooMuchData);
        }

        Ok(Self {
            reporter,
            epoch,
            slots,
            signature,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize)]
pub struct RaiEpochCloseReq {
    pub epoch: RaiEpoch,
    pub start_index: u32,
    pub max_entries: u16,
}

impl RaiEpochCloseReq {
    pub fn new(epoch: RaiEpoch, start_index: u32) -> Self {
        Self {
            epoch,
            start_index,
            max_entries: RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES,
        }
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let epoch = read_u64_be(reader)?;
        let start_index = crate::read_u32_be(reader)?;
        let max_entries = read_u16_be(reader)?;
        if max_entries == 0 || max_entries > RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            epoch,
            start_index,
            max_entries,
        })
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(&self.epoch.to_be_bytes())?;
        writer.write_all(&self.start_index.to_be_bytes())?;
        writer.write_all(&self.max_entries.to_be_bytes())
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RaiEpochCloseAck {
    pub page: Option<RaiEpochClosePage>,
}

impl RaiEpochCloseAck {
    pub fn unavailable() -> Self {
        Self { page: None }
    }

    pub fn new(page: RaiEpochClosePage) -> Self {
        Self { page: Some(page) }
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let available = read_u8(&mut bytes)?;
        match available {
            0 => {
                if !bytes.is_empty() {
                    return Err(DeserializationError::TooMuchData);
                }
                Ok(Self::unavailable())
            }
            1 => {
                let page = RaiEpochClosePage::deserialize(&mut bytes)?;
                if !bytes.is_empty() {
                    return Err(DeserializationError::TooMuchData);
                }
                Ok(Self::new(page))
            }
            _ => Err(DeserializationError::InvalidData),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match &self.page {
            Some(page) => {
                writer.write_all(&[1])?;
                page.serialize(writer)
            }
            None => writer.write_all(&[0]),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RaiEpochClosePage {
    pub epoch: RaiEpoch,
    pub total_entries: u32,
    pub start_index: u32,
    pub close_hash: BlockHash,
    pub entries: Vec<RaiEpochCloseEntry>,
}

impl RaiEpochClosePage {
    pub fn new(
        epoch: RaiEpoch,
        total_entries: u32,
        start_index: u32,
        close_hash: BlockHash,
        entries: Vec<RaiEpochCloseEntry>,
    ) -> Self {
        assert!(entries.len() <= RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES as usize);
        Self {
            epoch,
            total_entries,
            start_index,
            close_hash,
            entries,
        }
    }

    fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let epoch = read_u64_be(reader)?;
        let total_entries = crate::read_u32_be(reader)?;
        let start_index = crate::read_u32_be(reader)?;
        let close_hash = BlockHash::deserialize(reader)?;
        let count = read_u16_be(reader)? as usize;
        if count > RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES as usize {
            return Err(DeserializationError::TooMuchData);
        }
        let end_index = start_index
            .checked_add(count as u32)
            .ok_or(DeserializationError::InvalidData)?;
        if end_index > total_entries {
            return Err(DeserializationError::InvalidData);
        }

        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(RaiEpochCloseEntry::deserialize(reader)?);
        }

        Ok(Self {
            epoch,
            total_entries,
            start_index,
            close_hash,
            entries,
        })
    }

    fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        debug_assert!(self.entries.len() <= RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES as usize);
        writer.write_all(&self.epoch.to_be_bytes())?;
        writer.write_all(&self.total_entries.to_be_bytes())?;
        writer.write_all(&self.start_index.to_be_bytes())?;
        self.close_hash.serialize(writer)?;
        writer.write_all(&(self.entries.len() as u16).to_be_bytes())?;
        for entry in &self.entries {
            entry.serialize(writer)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RaiEpochCloseEntry {
    pub slot: RaiSlot,
    pub state: RaiEpochCloseEntryState,
}

impl RaiEpochCloseEntry {
    pub const SERIALIZED_SIZE: usize =
        RaiSlot::SERIALIZED_SIZE + std::mem::size_of::<u8>() + BlockHash::SERIALIZED_SIZE;

    fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let slot = RaiSlot::deserialize(reader)?;
        let state = RaiEpochCloseEntryState::deserialize(reader)?;
        Ok(Self { slot, state })
    }

    fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        self.slot.serialize(writer)?;
        self.state.serialize(writer)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RaiEpochCloseEntryState {
    Finalized(BlockHash),
    Carry(BlockHash),
    Released,
}

impl RaiEpochCloseEntryState {
    fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let kind = RaiEpochCloseEntryStateKind::from_u8(read_u8(reader)?)
            .ok_or(DeserializationError::InvalidData)?;
        let hash = BlockHash::deserialize(reader)?;
        match kind {
            RaiEpochCloseEntryStateKind::Finalized => Ok(Self::Finalized(hash)),
            RaiEpochCloseEntryStateKind::Carry => Ok(Self::Carry(hash)),
            RaiEpochCloseEntryStateKind::Released if hash.is_zero() => Ok(Self::Released),
            RaiEpochCloseEntryStateKind::Released => Err(DeserializationError::InvalidData),
        }
    }

    fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        match self {
            Self::Finalized(hash) => {
                writer.write_all(&[RaiEpochCloseEntryStateKind::Finalized as u8])?;
                hash.serialize(writer)
            }
            Self::Carry(hash) => {
                writer.write_all(&[RaiEpochCloseEntryStateKind::Carry as u8])?;
                hash.serialize(writer)
            }
            Self::Released => {
                writer.write_all(&[RaiEpochCloseEntryStateKind::Released as u8])?;
                BlockHash::ZERO.serialize(writer)
            }
        }
    }
}

#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum RaiEpochCloseEntryStateKind {
    Finalized = 0,
    Carry = 1,
    Released = 2,
}

fn read_u16_be<T>(reader: &mut T) -> std::io::Result<u16>
where
    T: Read,
{
    let mut buffer = [0; 2];
    reader.read_exact(&mut buffer)?;
    Ok(u16::from_be_bytes(buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_vote_validates_one_signature() {
        let key = PrivateKey::from(1);
        let vote = RaiVote::new_first(
            &key,
            RaiElectionId::Slot {
                slot: RaiSlot::new(Account::from(2), 3),
                epoch: 4,
            },
            RaiElectionValue::Block(BlockHash::from(5)),
        );

        vote.validate().unwrap();
    }

    #[test]
    fn pending_report_validates_signature() {
        let key = PrivateKey::from(1);
        let report = RaiPendingReport::new(
            &key,
            2,
            vec![
                RaiSlot::new(Account::from(3), 4),
                RaiSlot::new(Account::from(5), 6),
            ],
        );

        report.validate().unwrap();
    }

    #[test]
    fn epoch_close_ack_roundtrips_page() {
        let page = RaiEpochClosePage::new(
            3,
            2,
            0,
            BlockHash::from(7),
            vec![
                RaiEpochCloseEntry {
                    slot: RaiSlot::new(Account::from(1), 2),
                    state: RaiEpochCloseEntryState::Finalized(BlockHash::from(3)),
                },
                RaiEpochCloseEntry {
                    slot: RaiSlot::new(Account::from(4), 5),
                    state: RaiEpochCloseEntryState::Released,
                },
            ],
        );
        let ack = RaiEpochCloseAck::new(page);
        let mut bytes = Vec::new();
        ack.serialize(&mut bytes).unwrap();

        assert_eq!(RaiEpochCloseAck::deserialize(&bytes).unwrap(), ack);
    }

    #[test]
    fn epoch_close_request_rejects_oversized_page() {
        let request = RaiEpochCloseReq {
            epoch: 1,
            start_index: 0,
            max_entries: RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES + 1,
        };
        let mut bytes = Vec::new();
        request.serialize(&mut bytes).unwrap();

        assert!(RaiEpochCloseReq::deserialize(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn rai_vote_wire_layout_is_stable() {
        let key = PrivateKey::from(1);
        let slot = RaiSlot::new(Account::from(2), 3);
        let value = BlockHash::from(5);
        let vote = RaiVote::new_first(
            &key,
            RaiElectionId::Slot { slot, epoch: 4 },
            RaiElectionValue::Block(value),
        );

        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), RaiVote::SERIALIZED_SIZE);
        let mut offset = 0;

        assert_eq!(
            &bytes[offset..offset + PublicKey::SERIALIZED_SIZE],
            key.public_key().as_bytes()
        );
        offset += PublicKey::SERIALIZED_SIZE;

        assert_eq!(bytes[offset], RAI_VOTE_KIND_FIRST);
        offset += 1;

        assert_eq!(bytes[offset], RAI_VOTE_SCOPE_ALL);
        offset += 1;

        assert_eq!(bytes[offset], 0);
        offset += 1;

        assert_eq!(bytes[offset], RAI_ELECTION_KIND_SLOT);
        offset += 1;

        assert_eq!(
            &bytes[offset..offset + Account::SERIALIZED_SIZE],
            slot.account.as_bytes()
        );
        offset += Account::SERIALIZED_SIZE;

        assert_eq!(
            &bytes[offset..offset + std::mem::size_of::<u64>()],
            slot.account_height.to_be_bytes()
        );
        offset += std::mem::size_of::<u64>();

        assert_eq!(
            &bytes[offset..offset + std::mem::size_of::<u64>()],
            4u64.to_be_bytes()
        );
        offset += std::mem::size_of::<u64>();

        assert_eq!(
            &bytes[offset..offset + std::mem::size_of::<u64>()],
            0u64.to_be_bytes()
        );
        offset += std::mem::size_of::<u64>();

        assert_eq!(bytes[offset], RAI_ELECTION_VALUE_KIND_BLOCK);
        offset += 1;

        assert_eq!(
            &bytes[offset..offset + BlockHash::SERIALIZED_SIZE],
            value.as_bytes()
        );
        offset += BlockHash::SERIALIZED_SIZE;

        assert_eq!(&bytes[offset..], vote.signature.as_bytes());
    }

    #[test]
    fn rai_vote_scope_is_signed() {
        let key = PrivateKey::from(1);
        let election_id = RaiElectionId::Slot {
            slot: RaiSlot::new(Account::from(2), 3),
            epoch: 4,
        };
        let value = RaiElectionValue::Block(BlockHash::from(5));
        let mut vote = RaiVote::new_first_scoped(&key, 1, election_id, value);

        vote.validate().unwrap();
        vote.scope = RaiVoteScope::Committee(0);

        assert!(vote.validate().is_err());
    }

    #[test]
    fn close_election_wire_layout_distinguishes_cut_and_record() {
        let close_cut = RaiElectionId::CloseCut {
            epoch: 7,
            attempt: 11,
        };
        let close_record = RaiElectionId::CloseRecord {
            epoch: 7,
            attempt: 11,
        };

        let mut cut_bytes = Vec::new();
        let mut record_bytes = Vec::new();
        close_cut.serialize(&mut cut_bytes).unwrap();
        close_record.serialize(&mut record_bytes).unwrap();

        assert_eq!(cut_bytes.len(), RaiElectionId::SERIALIZED_SIZE);
        assert_eq!(record_bytes.len(), RaiElectionId::SERIALIZED_SIZE);
        assert_eq!(cut_bytes[0], RAI_ELECTION_KIND_CLOSE_CUT);
        assert_eq!(record_bytes[0], RAI_ELECTION_KIND_CLOSE_RECORD);
        let mut cut_slice = cut_bytes.as_slice();
        let mut record_slice = record_bytes.as_slice();
        assert_eq!(
            RaiElectionId::deserialize(&mut cut_slice).unwrap(),
            close_cut
        );
        assert_eq!(
            RaiElectionId::deserialize(&mut record_slice).unwrap(),
            close_record
        );
    }

    #[test]
    fn close_value_wire_layout_distinguishes_cut_and_record_hashes() {
        let close_cut = RaiElectionValue::CloseCutHash(BlockHash::from(8));
        let close_record = RaiElectionValue::CloseRecordHash(BlockHash::from(8));

        let mut cut_bytes = Vec::new();
        let mut record_bytes = Vec::new();
        close_cut.serialize(&mut cut_bytes).unwrap();
        close_record.serialize(&mut record_bytes).unwrap();

        assert_eq!(cut_bytes.len(), RaiElectionValue::SERIALIZED_SIZE);
        assert_eq!(record_bytes.len(), RaiElectionValue::SERIALIZED_SIZE);
        assert_eq!(cut_bytes[0], RAI_ELECTION_VALUE_KIND_CLOSE_CUT_HASH);
        assert_eq!(record_bytes[0], RAI_ELECTION_VALUE_KIND_CLOSE_RECORD_HASH);
        let mut cut_slice = cut_bytes.as_slice();
        let mut record_slice = record_bytes.as_slice();
        assert_eq!(
            RaiElectionValue::deserialize(&mut cut_slice).unwrap(),
            close_cut
        );
        assert_eq!(
            RaiElectionValue::deserialize(&mut record_slice).unwrap(),
            close_record
        );
    }

    #[test]
    fn close_record_hash_commits_to_epoch_predecessor_and_frontiers() {
        let frontiers: BTreeMap<_, _> = [(Account::from(2), BlockHash::from(3))]
            .into_iter()
            .collect();
        let base = RaiCloseRecord::new(1, BlockHash::from(1), frontiers.clone());

        assert_ne!(
            base.hash(),
            RaiCloseRecord::new(9, BlockHash::from(1), frontiers.clone()).hash()
        );
        assert_ne!(
            base.hash(),
            RaiCloseRecord::new(1, BlockHash::from(9), frontiers.clone()).hash()
        );
        assert_ne!(
            base.hash(),
            RaiCloseRecord::new(
                1,
                BlockHash::from(1),
                [(Account::from(2), BlockHash::from(9))]
                    .into_iter()
                    .collect(),
            )
            .hash()
        );
    }

    #[test]
    fn close_record_wire_layout_is_stable() {
        let record = RaiCloseRecord::new(
            7,
            BlockHash::from(1),
            [
                (Account::from(2), BlockHash::from(3)),
                (Account::from(4), BlockHash::from(5)),
            ]
            .into_iter()
            .collect(),
        );
        let mut bytes = Vec::new();

        record.serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), record.serialized_size());
        let mut slice = bytes.as_slice();
        assert_eq!(RaiCloseRecord::deserialize(&mut slice).unwrap(), record);
        assert!(slice.is_empty());
    }

    #[test]
    fn rai_pending_report_wire_layout_is_stable() {
        let key = PrivateKey::from(1);
        let slots = vec![RaiSlot::new(Account::from(2), 3)];
        let report = RaiPendingReport::new(&key, 4, slots.clone());

        let mut bytes = Vec::new();
        report.serialize(&mut bytes).unwrap();

        assert_eq!(bytes.len(), RaiPendingReport::serialized_size(slots.len()));
        let mut offset = 0;

        assert_eq!(
            &bytes[offset..offset + PublicKey::SERIALIZED_SIZE],
            key.public_key().as_bytes()
        );
        offset += PublicKey::SERIALIZED_SIZE;

        assert_eq!(
            &bytes[offset..offset + std::mem::size_of::<u64>()],
            4u64.to_be_bytes()
        );
        offset += std::mem::size_of::<u64>();

        assert_eq!(
            &bytes[offset..offset + Signature::SERIALIZED_SIZE],
            report.signature.as_bytes()
        );
        offset += Signature::SERIALIZED_SIZE;

        assert_eq!(
            &bytes[offset..offset + Account::SERIALIZED_SIZE],
            slots[0].account.as_bytes()
        );
        offset += Account::SERIALIZED_SIZE;

        assert_eq!(
            &bytes[offset..offset + std::mem::size_of::<u64>()],
            slots[0].account_height.to_be_bytes()
        );
        offset += std::mem::size_of::<u64>();

        assert_eq!(offset, bytes.len());
    }
}
