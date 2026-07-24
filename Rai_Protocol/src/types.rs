use std::fmt;

use crate::crypto::sha256;

pub type Epoch = u64;
pub type Round = u32;
pub type ReplicaId = u64;
pub type AccountId = u64;
pub type CommitteeId = u64;
pub type Amount = u128;
pub type Weight = u128;

/// A logical account-chain position.
///
/// Slots are account-scoped: sequence 1 is the first block after that
/// account's genesis, sequence 2 extends sequence 1, and so on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Slot {
    pub account: AccountId,
    pub sequence: u64,
}

impl Slot {
    pub const fn new(account: AccountId, sequence: u64) -> Self {
        Self { account, sequence }
    }

    pub fn encode(self, out: &mut Vec<u8>) {
        put_u64(out, self.account);
        put_u64(out, self.sequence);
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({},{})", self.account, self.sequence)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Hash32(pub [u8; 32]);

impl Hash32 {
    pub const ZERO: Self = Self([0u8; 32]);

    pub fn digest(bytes: &[u8]) -> Self {
        sha256(bytes)
    }

    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    pub fn short(self) -> String {
        self.to_hex()[..12].to_owned()
    }
}

impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ElectionId {
    Slot { slot: Slot, epoch: Epoch },
    CloseCut { epoch: Epoch, round: Round },
    CloseRecord { epoch: Epoch, round: Round },
}

impl ElectionId {
    pub fn epoch(&self) -> Epoch {
        match self {
            Self::Slot { epoch, .. }
            | Self::CloseCut { epoch, .. }
            | Self::CloseRecord { epoch, .. } => *epoch,
        }
    }

    pub fn slot(&self) -> Option<Slot> {
        match self {
            Self::Slot { slot, .. } => Some(*slot),
            _ => None,
        }
    }

    pub fn is_close(&self) -> bool {
        matches!(self, Self::CloseCut { .. } | Self::CloseRecord { .. })
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Slot { slot, epoch } => {
                out.push(0);
                slot.encode(out);
                put_u64(out, *epoch);
            }
            Self::CloseCut { epoch, round } => {
                out.push(1);
                put_u64(out, *epoch);
                put_u32(out, *round);
            }
            Self::CloseRecord { epoch, round } => {
                out.push(2);
                put_u64(out, *epoch);
                put_u32(out, *round);
            }
        }
    }
}

impl fmt::Display for ElectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Slot { slot, epoch } => write!(f, "Slot({slot},{epoch})"),
            Self::CloseCut { epoch, round } => write!(f, "CloseCut({epoch},{round})"),
            Self::CloseRecord { epoch, round } => write!(f, "CloseRecord({epoch},{round})"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum VoteValue {
    Candidate(Hash32),
    Timeout,
}

impl VoteValue {
    pub fn candidate(self) -> Option<Hash32> {
        match self {
            Self::Candidate(hash) => Some(hash),
            Self::Timeout => None,
        }
    }

    pub fn encode(self, out: &mut Vec<u8>) {
        match self {
            Self::Candidate(hash) => {
                out.push(0);
                out.extend_from_slice(&hash.0);
            }
            Self::Timeout => out.push(1),
        }
    }
}

impl fmt::Display for VoteValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Candidate(hash) => write!(f, "{}", hash.short()),
            Self::Timeout => write!(f, "timeout"),
        }
    }
}

pub(crate) fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

pub(crate) fn put_u128(out: &mut Vec<u8>, value: u128) {
    out.extend_from_slice(&value.to_be_bytes());
}
