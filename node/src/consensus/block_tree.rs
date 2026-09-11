use rsnano_types::{BlockHash, QualifiedRoot, RaiBlockTreeEntry};
use std::collections::{BTreeMap, HashMap};

/// Node-lifetime consensus block tree for continuously online replicas. Blocks
/// carry their account-chain parent links; forks remain separate from the ledger.
/// Timeout-only epochs have no payload or account-frontier effect.
#[derive(Default)]
pub struct RaiBlockTree {
    entries: BTreeMap<(QualifiedRoot, u64, BlockHash), RaiBlockTreeEntry>,
    roots_by_hash: HashMap<BlockHash, QualifiedRoot>,
}

impl RaiBlockTree {
    /// Merge monotonically. Different notarized forks may coexist; conflicting
    /// finalizations and finalization/timeout in one epoch may not.
    pub fn insert(&mut self, entry: RaiBlockTreeEntry) -> Result<bool, &'static str> {
        if !entry.is_valid() {
            return Err("invalid block-tree entry");
        }
        let related = self
            .entries
            .range(
                (entry.root.clone(), 0, BlockHash::ZERO)
                    ..=(entry.root.clone(), u64::MAX, BlockHash::MAX),
            )
            .map(|(_, e)| e);
        for old in related {
            if old.finalized && entry.finalized && old.hash() != entry.hash() {
                return Err("conflicting finalized blocks");
            }
            if old.epoch == entry.epoch
                && ((old.finalized && entry.block.is_none())
                    || (entry.finalized && old.block.is_none()))
            {
                return Err("timeout and finalization in the same epoch");
            }
        }
        let key = (
            entry.root.clone(),
            entry.epoch,
            entry.block.as_ref().map(|b| b.hash()).unwrap_or_default(),
        );
        if let Some(old) = self.entries.get_mut(&key) {
            if entry.finalized && !old.finalized {
                old.finalized = true;
                return Ok(true);
            }
            return Ok(false);
        }
        if entry.block.is_some() {
            self.roots_by_hash.insert(key.2, entry.root.clone());
        }
        self.entries.insert(key, entry);
        Ok(true)
    }

    /// Root of a certified block, for peers that know only its hash.
    pub fn root_of(&self, hash: &BlockHash) -> Option<&QualifiedRoot> {
        self.roots_by_hash.get(hash)
    }

    pub fn entries(&self) -> impl Iterator<Item = &RaiBlockTreeEntry> {
        self.entries.values()
    }

    pub fn for_root(&self, root: &QualifiedRoot) -> Vec<RaiBlockTreeEntry> {
        self.entries
            .range((root.clone(), 0, BlockHash::ZERO)..=(root.clone(), u64::MAX, BlockHash::MAX))
            .map(|(_, entry)| entry.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{SavedBlock, StateBlockArgs};

    #[test]
    fn retains_notarized_forks_and_monotonic_finalization_by_epoch() {
        let block = SavedBlock::new_test_instance();
        let mut tree = RaiBlockTree::default();
        let first = RaiBlockTreeEntry::notarized(block.clone().into(), 7);
        assert_eq!(tree.insert(first.clone()), Ok(true));
        assert_eq!(tree.insert(first.clone()), Ok(false));
        let mut finalized = first.clone();
        finalized.finalized = true;
        assert_eq!(tree.insert(finalized), Ok(true));
        assert_eq!(tree.insert(first), Ok(false));
        assert!(tree.for_root(&block.qualified_root())[0].finalized);
        assert_eq!(
            tree.insert(RaiBlockTreeEntry::notarized(block.clone().into(), 8)),
            Ok(true)
        );
        assert_eq!(tree.for_root(&block.qualified_root()).len(), 2);
        assert_eq!(tree.root_of(&block.hash()), Some(&block.qualified_root()));
        assert_eq!(tree.root_of(&BlockHash::from(1)), None);
    }

    #[test]
    fn timeout_has_no_block_and_cannot_conflict_with_same_epoch_finalization() {
        let block = SavedBlock::new_test_instance();
        let mut tree = RaiBlockTree::default();
        tree.insert(RaiBlockTreeEntry::timeout(
            block.qualified_root(),
            7,
            block.hash(),
        ))
        .unwrap();
        assert!(tree.entries().next().unwrap().block.is_none());
        let mut entry = RaiBlockTreeEntry::notarized(block.into(), 7);
        entry.finalized = true;
        assert!(tree.insert(entry.clone()).is_err());
        entry.epoch = 8;
        assert!(tree.insert(entry).is_ok());
    }

    #[test]
    fn multiple_notarizations_are_allowed_but_finalized_forks_are_rejected() {
        let args = StateBlockArgs::new_test_instance();
        let mut tree = RaiBlockTree::default();
        let mut a = RaiBlockTreeEntry::notarized(args.clone().into(), 0);
        let mut b = RaiBlockTreeEntry::notarized(
            StateBlockArgs {
                representative: 999.into(),
                ..args
            }
            .into(),
            0,
        );
        tree.insert(a.clone()).unwrap();
        tree.insert(b.clone()).unwrap();
        assert_eq!(tree.for_root(&a.root).len(), 2);
        a.finalized = true;
        tree.insert(a).unwrap();
        b.finalized = true;
        b.epoch = 1;
        assert!(tree.insert(b).is_err());
    }
}
