use crate::{Block, BlockHash, QualifiedRoot};
use serde::{Deserialize, Serialize};

/// A locally established consensus outcome, indexed independently of the account
/// ledger so notarized forks and timeout-only epochs do not change balances.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaiBlockTreeEntry {
    pub root: QualifiedRoot,
    pub epoch: u64,
    pub block: Option<Block>,
    pub finalized: bool,
    /// Routing candidate for timeout votes; it is not a notarized block.
    pub timeout_hash: BlockHash,
}

impl RaiBlockTreeEntry {
    pub fn notarized(block: Block, epoch: u64) -> Self {
        Self {
            root: block.qualified_root(),
            epoch,
            block: Some(block),
            finalized: false,
            timeout_hash: BlockHash::ZERO,
        }
    }

    pub fn timeout(root: QualifiedRoot, epoch: u64, hash: BlockHash) -> Self {
        Self {
            root,
            epoch,
            block: None,
            finalized: false,
            timeout_hash: hash,
        }
    }

    pub fn hash(&self) -> BlockHash {
        self.block
            .as_ref()
            .map(|b| b.hash())
            .unwrap_or(self.timeout_hash)
    }

    pub fn is_valid(&self) -> bool {
        match &self.block {
            Some(block) => block.qualified_root() == self.root,
            None => !self.finalized,
        }
    }
}
