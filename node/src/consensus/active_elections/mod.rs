mod active_elections_container;
mod aec_service;
mod apply_vote_helper;
mod cooldown_controller;
mod epoch_close;
mod epoch_committees;
mod epoch_states;
mod recently_confirmed_cache;
mod root_container;
mod slot_states;
mod stats;
mod vote_records;
mod vote_router;

pub use active_elections_container::*;
pub use aec_service::{AecService, AecSnapshot, BucketSnapshot};
pub use cooldown_controller::AecCooldownReason;
pub use epoch_close::EpochCloseInfo;
pub use epoch_committees::CommitteeInfo;

use std::{collections::HashMap, isize, sync::Arc, time::Duration};

use rsnano_types::{
    Account, Amount, Block, BlockHash, BlockPriority, ConsensusEpoch, QualifiedRoot, SavedBlock,
    TimePriority, VoteError,
};

use super::{
    ReceivedVote,
    election::{ConfirmedElection, Election, ElectionBehavior, ElectionId},
};
use root_container::{Entry, RootContainer};

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveElectionsConfig {
    /// Maximum number of simultaneous active elections (AEC size)
    pub max_elections: usize,
    /// Maximum cache size for recently_confirmed
    pub confirmation_cache: usize,
    /// RAI: advance to the next consensus epoch once this many elections of
    /// the current epoch have got a certificate; 0 never advances
    pub epoch_terminated_elections: usize,
    /// RAI: an epoch ends this long after its first election started; zero
    /// never ends an epoch by time
    pub epoch_duration: Duration,
    /// RAI: Δ_E of a slot of an epoch's close election: a replica first
    /// votes the timeout value once it waited this long for a valid
    /// proposal. A slot led by a representative which does not propose
    /// costs this long; a leader proposes as soon as it can derive a value
    pub close_round_timeout: Duration,
    /// RAI: whether this node casts account votes. A representative that
    /// does not still receives the epoch's votes, signs its report at the
    /// boundary and votes in the close: present for the handoff, absent
    /// from the voting. For the benchmark's silent representative.
    pub account_voting: bool,
    /// RAI: the checkpoint-finalization rule (see
    /// doc/benchmarks/cross-epoch-minimal/CHECKPOINT-FINALIZATION-VARIANT.md).
    /// The paper's certificate-only rule by default.
    pub checkpoint_finalization: crate::consensus::election::CheckpointFinalization,
    /// RAI: how committees count: the baseline's weighted joint agreement,
    /// or the paper's equal-weight members with explicit f and p and the
    /// closing committee deciding alone
    pub committee_model: crate::consensus::election::CommitteeModel,
}

impl Default for ActiveElectionsConfig {
    fn default() -> Self {
        Self {
            max_elections: 5000,
            confirmation_cache: 65536,
            epoch_terminated_elections: 0,
            epoch_duration: Duration::ZERO,
            close_round_timeout: Duration::from_secs(2),
            account_voting: true,
            checkpoint_finalization: Default::default(),
            committee_model: Default::default(),
        }
    }
}

pub enum AecFact {
    ElectionStarted(BlockHash, QualifiedRoot),
    ElectionConfirmed(ConfirmedElection),

    /// Ended ether confirmed or unconfirmed
    ElectionEnded(Election),

    /// Kudzu: the election holds a certificate and no longer occupies a
    /// slot in its priority bucket
    ElectionTerminated(ElectionId),

    /// RAI: new elections are now started in this epoch, and the report of
    /// the epoch left was taken at the switch, under the same lock that
    /// stopped its signing
    EpochAdvanced(ConsensusEpoch, Option<Arc<EpochReport>>),

    /// RAI: instances of a closed epoch opened after its certificate was
    /// seen got notarized: their blocks are not in the value finalized and
    /// are discarded, rolled back from the ledger
    LateBlocksDiscarded {
        epoch: ConsensusEpoch,
        hashes: Vec<BlockHash>,
    },

    /// RAI: the decided checkpoint of an epoch finalized these blocks beyond
    /// its predecessor. Installed: cemented in the ledger, where they are
    /// not already
    /// RAI: the unresolved branches a decided checkpoint retains, parents
    /// before children. A ledger that holds an omitted rival at one of these
    /// positions follows the checkpoint: the rival is rolled back and the
    /// retained block installed, so that the owner's fresh child can attach
    CheckpointRetained {
        epoch: ConsensusEpoch,
        retained: Vec<(Account, u64, BlockHash)>,
    },
    CheckpointFinalized {
        epoch: ConsensusEpoch,
        hashes: Vec<BlockHash>,
    },

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
}

impl AecFact {
    /// Diagnostic: the name of the fact's kind
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ElectionStarted(..) => "election_started",
            Self::ElectionConfirmed(..) => "election_confirmed",
            Self::ElectionEnded(..) => "election_ended",
            Self::ElectionTerminated(..) => "election_terminated",
            Self::EpochAdvanced(..) => "epoch_advanced",
            Self::LateBlocksDiscarded { .. } => "late_blocks_discarded",
            Self::CheckpointRetained { .. } => "checkpoint_retained",
            Self::CheckpointFinalized { .. } => "checkpoint_finalized",
            Self::BlockAddedToElection(..) => "block_added",
            Self::BlockDiscarded(..) => "block_discarded",
            Self::BlockConfirmed(..) => "block_confirmed",
            Self::WinnerChanged(..) => "winner_changed",
            Self::VoteProcessed(..) => "vote_processed",
            Self::Recovered => "recovered",
        }
    }
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum AecInsertError {
    Stopped,
    Duplicate,
    /// RAI: the current epoch has ended and drains; new elections start
    /// once the next epoch has started
    Draining,
    /// RAI: the block's position is held as a retained fork by the decided
    /// checkpoint; "a checkpoint never reopens a retained position", the
    /// owner resolves it with a child
    Retained,

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
