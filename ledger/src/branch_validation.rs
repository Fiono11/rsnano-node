//! Read-only validation of a consensus branch, independent of the installed ledger fork.
use std::collections::{HashMap, HashSet};

use crate::{AnySet, BlockError, Ledger, LedgerSet, block_insertion::BlockValidator};
use rsnano_types::{
    AccountInfo, Block, BlockHash, PendingInfo, PendingKey, SavedBlock, UnixMillisTimestamp,
};

#[derive(Debug, PartialEq, Eq)]
pub enum BranchError {
    Missing(BlockHash),
    FinalizedConflict(BlockHash, BlockHash),
    Invalid(BlockHash, BlockError),
    CycleOrLimit,
}

impl Ledger {
    /// Replay the dependency DAG into private account/pending maps. This never writes the
    /// ledger, adjusts representative weights, or emits rollback/processing events.
    pub fn validate_branch(
        &self,
        target: BlockHash,
        retained: impl FnMut(&BlockHash) -> Option<Block>,
        finalized: impl FnMut(rsnano_types::SlotRoot) -> Option<BlockHash>,
    ) -> Result<(), BranchError> {
        self.validated_branch_blocks(target, retained, finalized)
            .map(|_| ())
    }

    /// Validated dependency order for installing a branch after finalization.
    pub fn validated_branch_blocks(
        &self,
        target: BlockHash,
        mut retained: impl FnMut(&BlockHash) -> Option<Block>,
        mut finalized: impl FnMut(rsnano_types::SlotRoot) -> Option<BlockHash>,
    ) -> Result<Vec<Block>, BranchError> {
        let mut ordered = Vec::new();
        let any = self.any();
        let confirmed = self.confirmed();
        let genesis = self.genesis().clone();
        let mut blocks = HashMap::from([(genesis.hash(), genesis.clone())]);
        let mut accounts = HashMap::from([(
            genesis.account(),
            AccountInfo {
                head: genesis.hash(),
                open_block: genesis.hash(),
                balance: genesis.balance(),
                representative: genesis.representative_field().unwrap(),
                block_count: 1,
                ..Default::default()
            },
        )]);
        let mut pending: HashMap<PendingKey, PendingInfo> = HashMap::new();
        let mut visiting = HashSet::new();
        let mut stack = vec![(target, false)];
        let mut bodies = HashMap::new();
        while let Some((hash, expanded)) = stack.pop() {
            if blocks.contains_key(&hash) {
                continue;
            }
            if blocks.len() + visiting.len() > 100_000 {
                return Err(BranchError::CycleOrLimit);
            }
            let block = bodies
                .entry(hash)
                .or_insert_with(|| {
                    retained(&hash).or_else(|| any.get_block(&hash).map(Block::from))
                })
                .clone()
                .ok_or(BranchError::Missing(hash))?;
            if !expanded {
                if !visiting.insert(hash) {
                    return Err(BranchError::CycleOrLimit);
                }
                let winner = finalized(block.qualified_root().slot()).or_else(|| {
                    any.block_successor_by_qualified_root(&block.qualified_root())
                        .filter(|h| confirmed.block_exists(h))
                });
                if let Some(winner) = winner.filter(|winner| *winner != hash) {
                    return Err(BranchError::FinalizedConflict(hash, winner));
                }
                stack.push((hash, true));
                if !block.previous().is_zero() && !blocks.contains_key(&block.previous()) {
                    stack.push((block.previous(), false));
                }
                continue;
            }
            let previous = blocks.get(&block.previous()).cloned();
            let source = match &block {
                Block::State(_)
                    if self
                        .constants
                        .epochs
                        .is_epoch_link(&block.link_field().unwrap_or_default()) =>
                {
                    BlockHash::ZERO
                }
                Block::State(_)
                    if block.balance_field().unwrap()
                        > previous.as_ref().map(|b| b.balance()).unwrap_or_default() =>
                {
                    block.source_or_link()
                }
                Block::LegacyReceive(_) | Block::LegacyOpen(_) => block.source_or_link(),
                _ => BlockHash::ZERO,
            };
            if !source.is_zero() && !blocks.contains_key(&source) {
                stack.push((hash, true));
                stack.push((source, false));
                continue;
            }
            let account = block
                .account_field()
                .or_else(|| previous.as_ref().map(|b| b.account()))
                .unwrap_or_default();
            let validator = BlockValidator {
                block: &block,
                epochs: &self.constants.epochs,
                work: &self.constants.work,
                existing_block: None,
                account,
                previous_block: previous,
                old_account_info: accounts.get(&account).cloned(),
                pending_receive_info: pending.get(&PendingKey::new(account, source)).cloned(),
                any_pending_exists: pending.keys().any(|key| key.receiving_account == account),
                source_block_exists: !source.is_zero() && blocks.contains_key(&source),
                now: UnixMillisTimestamp::now(),
            };
            let saved = any.get_block(&hash);
            let instructions = validator
                .validate_with_saved_block(saved.as_ref())
                .map_err(|e| BranchError::Invalid(hash, e))?;
            accounts.insert(account, instructions.set_account_info);
            if let Some(key) = instructions.delete_pending {
                pending.remove(&key);
            }
            if let Some((key, value)) = instructions.insert_pending {
                pending.insert(key, value);
            }
            ordered.push(block.clone());
            blocks.insert(hash, SavedBlock::new(block, instructions.set_sideband));
            visiting.remove(&hash);
        }
        Ok(ordered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::UnsavedBlockLatticeBuilder;
    use rsnano_types::{PrivateKey, Signature};

    #[test]
    fn validates_rolled_back_branch_without_modifying_installed_fork() {
        let ledger = Ledger::new_null();
        let mut chain = UnsavedBlockLatticeBuilder::new();
        let mut fork = chain.clone();
        let b = chain.genesis().send(100, 10);
        let c = chain.genesis().send(101, 1);
        let a = fork.genesis().send(102, 10);
        ledger.process_one(&b).unwrap();
        ledger.process_one(&c).unwrap();
        ledger.roll_back_competitors([&a]);
        ledger.process_one(&a).unwrap();
        let bodies = HashMap::from([(b.hash(), b.clone()), (c.hash(), c.clone())]);
        let count = ledger.block_count();
        assert_eq!(
            ledger.validate_branch(c.hash(), |h| bodies.get(h).cloned(), |_| None),
            Ok(())
        );
        assert_eq!(ledger.block_count(), count);
        assert!(ledger.any().block_exists(&a.hash()));
        assert!(!ledger.any().block_exists(&b.hash()));
        assert!(!ledger.any().block_exists(&c.hash()));
        ledger.confirm(a.hash());
        assert_eq!(
            ledger.validate_branch(c.hash(), |h| bodies.get(h).cloned(), |_| None),
            Err(BranchError::FinalizedConflict(b.hash(), a.hash()))
        );
    }

    #[test]
    fn missing_predecessor_is_recoverable_not_invalid() {
        let ledger = Ledger::new_null();
        let mut chain = UnsavedBlockLatticeBuilder::new();
        let b = chain.genesis().send(100, 10);
        let c = chain.genesis().send(101, 1);
        assert_eq!(
            ledger.validate_branch(c.hash(), |h| (*h == c.hash()).then(|| c.clone()), |_| None),
            Err(BranchError::Missing(b.hash()))
        );
    }

    #[test]
    fn installed_hash_does_not_authorize_a_different_signature() {
        let ledger = Ledger::new_null();
        let mut chain = UnsavedBlockLatticeBuilder::new();
        let valid = chain.genesis().send(100, 10);
        ledger.process_one(&valid).unwrap();
        assert_eq!(ledger.validate_branch(valid.hash(), |_| None, |_| None), Ok(()));
        let mut bad = valid.clone();
        bad.set_signature(Signature::new());
        assert_eq!(bad.hash(), valid.hash());
        assert!(matches!(
            ledger.validate_branch(bad.hash(), |h| (*h == bad.hash()).then(|| bad.clone()), |_| None),
            Err(BranchError::Invalid(_, BlockError::BadSignature))
        ));
    }

    #[test]
    fn validates_receive_source_and_rejects_bad_signature() {
        let ledger = Ledger::new_null();
        let key = PrivateKey::from(11);
        let mut chain = UnsavedBlockLatticeBuilder::new();
        let send = chain.genesis().send(key.account(), 10);
        let receive = chain.account(&key).receive(&send);
        let bodies = HashMap::from([(send.hash(), send), (receive.hash(), receive.clone())]);
        assert_eq!(
            ledger.validate_branch(receive.hash(), |h| bodies.get(h).cloned(), |_| None),
            Ok(())
        );
        let mut bad = receive.clone();
        bad.set_signature(Signature::new());
        assert!(matches!(
            ledger.validate_branch(
                bad.hash(),
                |h| if *h == bad.hash() {
                    Some(bad.clone())
                } else {
                    bodies.get(h).cloned()
                },
                |_| None
            ),
            Err(BranchError::Invalid(_, BlockError::BadSignature))
        ));
    }
}
