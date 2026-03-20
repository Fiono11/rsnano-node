#[macro_use]
extern crate anyhow;

#[macro_use]
extern crate strum_macros;

mod block_cementer;
mod block_dependencies_finder;
mod block_insertion;
mod block_rollback;
mod ledger;
mod ledger_builder;
mod ledger_constants;
mod ledger_inserter;
mod ledger_sets;
mod rep_weight_cache;
mod rep_weights_updater;
mod representative_block_finder;
pub mod test_helpers;
mod vote_verifier;

#[cfg(test)]
mod ledger_tests;

pub use block_dependencies_finder::*;
pub(crate) use block_rollback::BlockRollbackPerformer;
pub use block_rollback::RollbackError;
pub use ledger::*;
pub use ledger_builder::*;
pub use ledger_constants::{
    DEV_GENESIS_ACCOUNT, DEV_GENESIS_BLOCK, DEV_GENESIS_HASH, DEV_GENESIS_PUB_KEY, LedgerConstants,
};
pub use ledger_inserter::*;
pub use ledger_sets::*;
pub use rep_weight_cache::*;
pub use rep_weights_updater::*;
pub(crate) use representative_block_finder::RepresentativeBlockFinder;
use rsnano_types::{Block, BlockHash, BlockPriority, SavedBlock};

pub enum LedgerEvent {
    /// The confirmed block + it's confirmation root
    BlocksProcessed(Vec<ProcessResult>),
    BlocksConfirmed(Vec<(SavedBlock, BlockHash)>),
    BlocksRolledBack(RollbackResults),
}

/// Result of a processed block
#[derive(Clone, Debug)]
pub struct ProcessResult {
    pub block: Block,
    pub source: BlockSource,
    pub status: Result<(), BlockError>,
    pub saved_block: Option<SavedBlock>,
    pub priority: BlockPriority,
}

#[derive(
    Copy, Clone, PartialEq, Eq, Debug, PartialOrd, Ord, EnumIter, EnumCount, Hash, IntoStaticStr,
)]
#[strum(serialize_all = "snake_case")]
pub enum BlockSource {
    Live,
    LiveOriginator,
    Bootstrap,
    Unchecked,
    Local,
    Forced,
    Election,
}
