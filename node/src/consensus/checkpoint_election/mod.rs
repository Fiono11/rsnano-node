//! Fast Archipelago v1.3 checkpoint protocol components and election adapter.
//!
//! Application dissemination, recursive proof verification and transport are
//! integrated in per-instance sessions with opt-in all-online benchmark wiring.
//! The round synchronizer remains outstanding. Missing payloads or certificate
//! evidence never count as valid.
mod application_exchange;
mod benchmark_service;
mod application_wire;
mod archipelago;
mod candidate;
mod candidate_admission;
mod decision_proof;
mod driver;
mod election_adapter;
mod election_router;
mod fallback;
mod fast_wire;
mod first_votes;
mod initial_r;
mod observer;
mod participant;
mod recovery;
mod session;
mod slow_certificates;
mod slow_frames;
mod slow_io;
mod slow_proposer;
mod slow_replica;
mod slow_requests;
mod slow_responder;
mod slow_wire;
mod transport;

pub use application_exchange::*;
pub(crate) use benchmark_service::*;
pub use application_wire::*;
pub use archipelago::*;
pub use candidate::*;
pub use candidate_admission::*;
pub use decision_proof::*;
pub use driver::*;
pub use election_adapter::*;
pub use election_router::*;
pub use fallback::*;
pub use fast_wire::*;
pub use first_votes::*;
pub use initial_r::*;
pub use observer::*;
pub use participant::*;
pub use recovery::*;
pub use session::*;
pub use slow_certificates::*;
pub use slow_frames::*;
pub use slow_io::SlowIoLimits;
pub use slow_proposer::*;
pub use slow_replica::*;
pub use slow_requests::*;
pub use slow_responder::*;
pub use slow_wire::SlowWireCodec;
pub use transport::*;

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
            .update(b"RAI checkpoint instance v1.3")
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
            .update(b"RAI checkpoint introduction v1.3")
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
    VerificationLimit,
    CyclicEvidence,
    WrongPhase,
}

/// Trust boundary to the application validator and the R/A/B certificate
/// engine (commit 8). Implementations must not accept bare signatures as proof.
/// CandidateEvidence implements this boundary using validated candidate admissions
/// and initial-R certificates; there is no permissive default.
pub trait CheckpointEvidence {
    /// Requires a reconstructed payload satisfying RAI Valid_I, not just a hash.
    fn validate_value(
        &self,
        instance: &CheckpointInstance,
        value: BlockHash,
    ) -> Result<(), CheckpointError>;

    /// Check the complete finite introduction chain for this exact value.
    /// Proposal: authenticate a rank-zero proposal in this instance.
    /// PreviousB/Fast are legacy encodings rejected by the fast context.
    /// Slow consensus uses a separate instance and certificate namespace.
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
        if rank != 0 {
            return Err(CheckpointError::WrongRank);
        }
        if !matches!(introduction, Introduction::Proposal(_)) {
            return Err(CheckpointError::WrongAncestry);
        }
        evidence.validate_value(&self.instance, value)?;
        evidence.validate_introduction(&self.instance, rank, value, introduction)
    }
}

#[cfg(test)]
mod tests;
