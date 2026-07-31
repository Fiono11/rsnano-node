mod active_elections_container;
mod aec_service;
mod apply_vote_helper;
mod cooldown_controller;
mod recently_confirmed_cache;
mod root_container;
mod stats;
mod vote_router;

pub use active_elections_container::*;
#[cfg(feature = "rai_protocol")]
pub use aec_service::RaiEpochTicker;
pub use aec_service::{AecService, AecSnapshot, BucketSnapshot};
pub use cooldown_controller::AecCooldownReason;

#[cfg(feature = "rai_protocol")]
use std::sync::Arc;
use std::{collections::HashMap, isize};

#[cfg(feature = "rai_protocol")]
use rsnano_ledger::RepWeights;
use rsnano_types::{
    Amount, Block, BlockHash, BlockPriority, QualifiedRoot, SavedBlock, TimePriority, VoteError,
};

use super::{
    ReceivedVote,
    election::{ConfirmedElection, Election, ElectionBehavior},
};
use root_container::{Entry, RootContainer};

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveElectionsConfig {
    /// Maximum number of simultaneous active elections (AEC size)
    pub max_elections: usize,
    /// Maximum cache size for recently_confirmed
    pub confirmation_cache: usize,
}

impl Default for ActiveElectionsConfig {
    fn default() -> Self {
        Self {
            max_elections: 5000,
            confirmation_cache: 65536,
        }
    }
}

pub enum AecFact {
    ElectionStarted(BlockHash, QualifiedRoot),
    ElectionConfirmed(ConfirmedElection),

    /// Ended ether confirmed or unconfirmed
    ElectionEnded(Election),

    BlockAddedToElection(BlockHash),
    BlockDiscarded(Block),
    BlockConfirmed(SavedBlock, ConfirmedElection),
    /// old winner + new winner block
    WinnerChanged(BlockHash, Block),

    VoteProcessed(
        ReceivedVote,
        Amount,
        HashMap<BlockHash, Result<(), VoteError>>,
    ),
    Recovered,
    #[cfg(feature = "rai_protocol")]
    RaiCloseInstalled(crate::consensus::rai::RaiFrontierMap),
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum AecInsertError {
    Stopped,
    Duplicate,
    MissingRaiGoverningClose,
    #[cfg(feature = "rai_protocol")]
    InvalidRaiCloseElection,

    /// This block or a fork got recently confirmed, so there is no need for a new election.
    RecentlyConfirmed,
}

#[derive(Default)]
pub struct ActiveElectionsInfo {
    pub max_elections: usize,
    pub total: usize,
    pub stale: usize,
    pub priority: usize,
    pub hinted: usize,
    pub optimistic: usize,
}

pub struct AecInsertRequest {
    pub block: SavedBlock,
    pub behavior: ElectionBehavior,
    pub priority: BlockPriority,
}

/// Explicit request for a synthetic, round-zero close-cut election.
#[cfg(feature = "rai_protocol")]
pub struct RaiCloseElectionSpec {
    pub id: crate::consensus::rai::RaiCloseElectionId,
    pub root: QualifiedRoot,
    pub candidate: BlockHash,
    pub committee: Arc<RepWeights>,
}

impl AecInsertRequest {
    pub fn new_hinted(block: SavedBlock, priority: BlockPriority) -> Self {
        Self {
            block,
            behavior: ElectionBehavior::Hinted,
            priority,
        }
    }

    pub fn new_optimistic(block: SavedBlock, priority: BlockPriority) -> Self {
        Self {
            block,
            behavior: ElectionBehavior::Optimistic,
            priority,
        }
    }

    pub fn new_manual(block: SavedBlock, priority: BlockPriority) -> Self {
        Self {
            block,
            behavior: ElectionBehavior::Manual,
            priority,
        }
    }

    pub fn new_priority(block: SavedBlock, priority: BlockPriority) -> Self {
        Self {
            block,
            behavior: ElectionBehavior::Priority,
            priority,
        }
    }
}

const AEC_STAT_KEY: &str = "active_elections";

/// Provides blocks for which an election should be scheduled
pub trait ElectionCandidateSource {
    fn should_schedule(&self, buckets: &[BucketInfo]) -> bool;

    fn next_candidate(
        &mut self,
        bucket_id: usize,
        vacancy: isize,
        lowest_priority: TimePriority,
    ) -> Option<ElectionCandidate>;
}

#[derive(Clone, PartialEq, Eq)]
pub struct BucketInfo {
    /// The lowest priority of all the elections which are currently in the bucket
    pub lowest_priority: BlockPriority,

    /// Number of elections which are currently in this bucket
    pub election_count: usize,

    /// Maximum number of elections in that bucket
    pub max_elections: usize,
}

impl BucketInfo {
    pub fn new(max_elections: usize) -> Self {
        Self {
            lowest_priority: BlockPriority::MIN,
            election_count: 0,
            max_elections,
        }
    }

    pub fn vacancy(&self) -> isize {
        self.max_elections as isize - self.election_count as isize
    }
}

pub struct ElectionCandidate {
    pub bucket_id: usize,
    pub block: SavedBlock,
    pub priority: BlockPriority,
}
