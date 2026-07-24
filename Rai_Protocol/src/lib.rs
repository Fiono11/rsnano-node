//! RAI protocol proof of concept.
//!
//! This crate implements the executable core of the uploaded “RAI Protocol:
//! Minimal Specification”: account/sequence slots, epoch-scoped elections,
//! first-valid receive voting with one active election per replica/slot,
//! Ed25519-authorized account blocks with balance, sends, receives, and delegation,
//! Ed25519-signed replica votes/reports, seeded delayed message delivery,
//! random-account block-tree proposals, asynchronous close-cut rounds,
//! per-committee certificates, completion/finality, and close-package resolution.

pub mod block;
pub mod certificate;
pub mod close;
pub mod committee;
pub mod crypto;
pub mod engine;
pub mod error;
pub mod simulation;
pub mod types;
pub mod vote;

pub use block::{
    hash_account_state, hash_ledger_frontiers, AccountState, Block, BlockStore, GenesisAccount,
    Receive, Send, SendId, SignedBlock, DEFAULT_GENESIS_BALANCE, DEFAULT_GENESIS_REPRESENTATIVE,
};
pub use certificate::{GlobalResult, LocalResult};
pub use close::{
    hash_close_cut, CertifiedCloseState, CloseCutCandidate, ClosePackage, CloseRecord, ElectionCut,
    FinalityEvidence, JointReportProof, ReleaseEvidence, SignedReport, SlotStatus,
};
pub use committee::Committee;
pub use crypto::{
    AccountKeyStore, CryptoProvider, DemoKeyStore, Ed25519KeyStore, Ed25519PublicKey, Signature,
};
pub use engine::{CloseProtocolAction, EpochState, GlobalResultUpdate, RaiEngine};
pub use error::{RaiError, Result};
pub use simulation::{
    run_timed_six_node_simulation, timed_simulation_help, ByzantineBehavior, EpochSnapshot,
    LogLevel, PartitionWindow, SimulationClient, SimulationReport, TimedSimulationConfig,
};
pub use types::{
    AccountId, Amount, CommitteeId, ElectionId, Epoch, Hash32, ReplicaId, Round, Slot, VoteValue,
    Weight,
};
pub use vote::{SignedVote, VoteKind, VotePool};
