use std::io::{Read, Write};

use num_traits::FromPrimitive;

use crate::{
    Account, Blake2HashBuilder, BlockHash, DeserializationError, PrivateKey, ProtocolInfo,
    PublicKey, Signature, SignatureError, read_u8, read_u64_be,
};

pub const RAI_PROTOCOL_VERSION: u8 = ProtocolInfo::RAI_PROTOCOL_VERSION;

pub const RAI_ELECTION_KIND_SLOT: u8 = 0;
pub const RAI_ELECTION_KIND_CLOSE: u8 = 1;

pub const RAI_VOTE_KIND_FIRST: u8 = 0;
pub const RAI_VOTE_KIND_NOTARIZATION: u8 = 1;
pub const RAI_VOTE_KIND_FINAL: u8 = 2;

pub const RAI_ELECTION_VALUE_KIND_BLOCK: u8 = 0;
pub const RAI_ELECTION_VALUE_KIND_CLOSE_HASH: u8 = 1;
pub const RAI_ELECTION_VALUE_KIND_TIMEOUT: u8 = 2;

pub const RAI_PENDING_REPORT_MAX_PAYLOAD_SIZE: usize = 65 * 1024;

const RAI_VOTE_HASH_PREFIX: &[u8] = b"rai vote ";
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
    Close {
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
            Self::Close { epoch, attempt } => {
                writer.write_all(&[RaiElectionKind::Close as u8])?;
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
            RaiElectionKind::Close => Ok(Self::Close { epoch, attempt }),
        }
    }
}

#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum RaiElectionKind {
    Slot = RAI_ELECTION_KIND_SLOT,
    Close = RAI_ELECTION_KIND_CLOSE,
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

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RaiElectionValue {
    Block(BlockHash),
    CloseHash(BlockHash),
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
            Self::CloseHash(hash) => {
                writer.write_all(&[RaiElectionValueKind::CloseHash as u8])?;
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
            RaiElectionValueKind::CloseHash => Ok(Self::CloseHash(hash)),
            RaiElectionValueKind::Timeout => Ok(Self::Timeout),
        }
    }
}

#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum RaiElectionValueKind {
    Block = RAI_ELECTION_VALUE_KIND_BLOCK,
    CloseHash = RAI_ELECTION_VALUE_KIND_CLOSE_HASH,
    Timeout = RAI_ELECTION_VALUE_KIND_TIMEOUT,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RaiVote {
    pub voter: PublicKey,
    pub kind: RaiVoteKind,
    pub election_id: RaiElectionId,
    pub value: RaiElectionValue,
    pub signature: Signature,
}

impl RaiVote {
    pub const SERIALIZED_SIZE: usize = PublicKey::SERIALIZED_SIZE
        + std::mem::size_of::<u8>()
        + RaiElectionId::SERIALIZED_SIZE
        + RaiElectionValue::SERIALIZED_SIZE
        + Signature::SERIALIZED_SIZE;

    pub fn new_first(
        key: &PrivateKey,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        Self::new_signed(key, RaiVoteKind::First, election_id, value)
    }

    pub fn new_notarization(
        key: &PrivateKey,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        Self::new_signed(key, RaiVoteKind::Notarization, election_id, value)
    }

    pub fn new_final(
        key: &PrivateKey,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        Self::new_signed(key, RaiVoteKind::Final, election_id, value)
    }

    fn new_signed(
        key: &PrivateKey,
        kind: RaiVoteKind,
        election_id: RaiElectionId,
        value: RaiElectionValue,
    ) -> Self {
        let signature = key.sign(Self::hash_for(kind, &election_id, &value).as_bytes());

        Self {
            voter: key.public_key(),
            kind,
            election_id,
            value,
            signature,
        }
    }

    pub fn hash(&self) -> BlockHash {
        Self::hash_for(self.kind, &self.election_id, &self.value)
    }

    pub fn hash_for(
        kind: RaiVoteKind,
        election_id: &RaiElectionId,
        value: &RaiElectionValue,
    ) -> BlockHash {
        let mut bytes = Vec::with_capacity(
            std::mem::size_of::<u8>()
                + RaiElectionId::SERIALIZED_SIZE
                + RaiElectionValue::SERIALIZED_SIZE,
        );
        bytes.push(kind as u8);
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
        self.election_id.serialize(writer)?;
        self.value.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let voter = PublicKey::deserialize(&mut bytes)?;
        let kind =
            RaiVoteKind::from_u8(read_u8(&mut bytes)?).ok_or(DeserializationError::InvalidData)?;
        let election_id = RaiElectionId::deserialize(&mut bytes)?;
        let value = RaiElectionValue::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        if !bytes.is_empty() {
            return Err(DeserializationError::TooMuchData);
        }

        Ok(Self {
            voter,
            kind,
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
