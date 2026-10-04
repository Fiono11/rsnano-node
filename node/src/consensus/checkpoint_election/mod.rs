//! Pure Fast Archipelago v1.2 first-vote and recovery logic.
//!
//! No node service is installed here. Full R/B ancestry and ordinary R
//! certificate validation are mandatory inputs from the later R/A/B engine;
//! missing application data or certificate evidence is never treated as valid.
mod archipelago;
mod first_votes;
mod recovery;

pub use archipelago::*;
pub use first_votes::*;
pub use recovery::*;

use rsnano_types::{Blake2HashBuilder, BlockHash, ConsensusEpoch};
use serde::{Deserialize, Serialize};

use super::election::{CheckpointThresholds, Committee};

/// Separates sessions, closing epochs, predecessor states and fault budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CheckpointInstance {
    pub session: BlockHash,
    pub epoch: ConsensusEpoch,
    pub predecessor: BlockHash,
    pub committee: BlockHash,
}

impl CheckpointInstance {
    pub fn digest(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"RAI checkpoint instance v1.2")
            .update(self.session.as_bytes())
            .update(self.epoch.as_u64().to_le_bytes())
            .update(self.predecessor.as_bytes())
            .update(self.committee.as_bytes())
            .build()
    }
}

/// References the legal introduction for the supported R value, which may
/// differ from the most recently received R request. The kind is signed too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Introduction {
    Proposal(BlockHash),
    PreviousB(BlockHash),
    PreviousFast(BlockHash),
}

impl Introduction {
    pub fn digest(&self) -> BlockHash {
        let (tag, hash) = match self {
            Self::Proposal(hash) => (0u8, hash),
            Self::PreviousB(hash) => (1, hash),
            Self::PreviousFast(hash) => (2, hash),
        };
        Blake2HashBuilder::new()
            .update(b"RAI checkpoint introduction v1.2")
            .update([tag])
            .update(hash.as_bytes())
            .build()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointError {
    InvalidCommittee,
    WrongInstance,
    WrongRank,
    NonMember,
    BadSignature,
    WrongAncestry,
    InvalidValue,
    InvalidEvidence,
    MissingEvidence(BlockHash),
    DuplicateSigner,
    InvalidSize,
    InvalidRecovery,
    JournalFailure,
    InvalidJournalRecord,
}

/// Trust boundary to the application validator and the R/A/B certificate
/// engine (commit 8). Implementations must not accept bare signatures as proof.
/// There is deliberately no default or production implementation in commit 7.
pub trait CheckpointEvidence {
    /// Requires a reconstructed payload satisfying RAI Valid_I, not just a hash.
    fn validate_value(
        &self,
        instance: &CheckpointInstance,
        value: BlockHash,
    ) -> Result<(), CheckpointError>;

    /// Check the complete finite introduction chain for this exact value.
    /// Proposal: authenticate a rank-zero proposal in this instance.
    /// PreviousB/Fast: verify a certificate at exactly rank-1 and recompute its
    /// carried value, including all recursive reliability/recovery checks.
    fn validate_introduction(
        &self,
        instance: &CheckpointInstance,
        rank: u64,
        value: BlockHash,
        introduction: &Introduction,
    ) -> Result<(), CheckpointError>;

    /// Independently verify Q distinct eligible R responses and their full
    /// ancestry/witnesses, and recompute the maximum at the selected rank.
    /// Reject evidence selecting another rank. A caller-supplied maximum is
    /// never sufficient to justify the empty recovery case.
    fn r_maximum(
        &self,
        instance: &CheckpointInstance,
        rank: u64,
        evidence: BlockHash,
    ) -> Result<BlockHash, CheckpointError>;
}

pub struct CheckpointContext {
    instance: CheckpointInstance,
    committee: Committee,
    thresholds: CheckpointThresholds,
}

impl CheckpointContext {
    pub fn new(
        instance: CheckpointInstance,
        committee: Committee,
    ) -> Result<Self, CheckpointError> {
        let thresholds = committee
            .checkpoint_thresholds()
            .ok_or(CheckpointError::InvalidCommittee)?;
        if committee.digest() != instance.committee {
            return Err(CheckpointError::WrongInstance);
        }
        Ok(Self {
            instance,
            committee,
            thresholds,
        })
    }

    pub fn instance(&self) -> CheckpointInstance {
        self.instance
    }
    pub fn thresholds(&self) -> CheckpointThresholds {
        self.thresholds
    }

    fn verify_introduction(
        &self,
        rank: u64,
        value: BlockHash,
        introduction: &Introduction,
        evidence: &impl CheckpointEvidence,
    ) -> Result<(), CheckpointError> {
        if (rank == 0) != matches!(introduction, Introduction::Proposal(_)) {
            return Err(CheckpointError::WrongAncestry);
        }
        evidence.validate_value(&self.instance, value)?;
        evidence.validate_introduction(&self.instance, rank, value, introduction)
    }
}

#[cfg(test)]
mod tests;
