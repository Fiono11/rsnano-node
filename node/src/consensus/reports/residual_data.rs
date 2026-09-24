use crate::consensus::{AecService, ForkCache, election::CertifiedBlock};
use rsnano_ledger::{AnySet, Ledger};
use rsnano_types::{Block, BlockBase, BlockHash, ConsensusEpoch};
use std::{
    collections::{BTreeMap, HashMap},
    ops::Deref,
    sync::{Arc, Mutex, RwLock},
};

/// Owner-signed block data retained independently of live election routing.
/// Placement supplies no certificate or eligibility/finality decision.
pub(super) struct ResidualData {
    ledger: Arc<Ledger>,
    forks: Arc<RwLock<ForkCache>>,
    aec: Arc<AecService>,
    retained: Mutex<BTreeMap<ConsensusEpoch, HashMap<BlockHash, Block>>>,
}
impl ResidualData {
    pub fn new(ledger: Arc<Ledger>, forks: Arc<RwLock<ForkCache>>, aec: Arc<AecService>) -> Self {
        Self {
            ledger,
            forks,
            aec,
            retained: Mutex::new(BTreeMap::new()),
        }
    }
    fn block(&self, hash: &BlockHash) -> Option<Block> {
        if let Some(block) = self.ledger.any().get_block(hash) {
            return Some(block.deref().clone());
        }
        if let Some(block) = self.aec.report_block(hash) {
            return Some(block);
        }
        if let Some(block) = self.forks.read().unwrap().block(hash) {
            return Some(block);
        }
        self.retained
            .lock()
            .unwrap()
            .values()
            .find_map(|blocks| blocks.get(hash).cloned())
    }
    pub fn retain(&self, epoch: ConsensusEpoch, hashes: impl IntoIterator<Item = BlockHash>) {
        for hash in hashes {
            if self
                .retained
                .lock()
                .unwrap()
                .get(&epoch)
                .is_some_and(|e| e.contains_key(&hash))
            {
                continue;
            }
            if let Some(block) = self.block(&hash) {
                self.retained
                    .lock()
                    .unwrap()
                    .entry(epoch)
                    .or_default()
                    .insert(hash, block);
            }
        }
        let mut retained = self.retained.lock().unwrap();
        while retained.len() > super::ReportExchange::MAX_EPOCHS {
            retained.pop_first();
        }
    }
    pub fn retained(&self, epoch: ConsensusEpoch, hashes: &[BlockHash]) -> Vec<Block> {
        let retained = self.retained.lock().unwrap();
        hashes
            .iter()
            .filter_map(|hash| retained.get(&epoch)?.get(hash).cloned())
            .collect()
    }
    pub fn placement(&self, hash: &BlockHash) -> Option<(CertifiedBlock, BlockHash)> {
        self.place(hash, 0)
    }
    fn place(&self, hash: &BlockHash, depth: usize) -> Option<(CertifiedBlock, BlockHash)> {
        if depth >= 256 {
            return None;
        }
        if let Some(block) = self.ledger.any().get_block(hash) {
            return Some((
                CertifiedBlock::new(block.account(), block.height(), *hash),
                block.previous(),
            ));
        }
        // Legacy/epoch-signer variants are deliberately not inferred here.
        let Block::State(block) = self.block(hash)? else {
            return None;
        };
        block.verify_signature().ok()?;
        let previous = block.previous();
        let height = if previous.is_zero() {
            1
        } else {
            let (parent, _) = self.place(&previous, depth + 1)?;
            if parent.account != block.account() {
                return None;
            }
            parent.height.checked_add(1)?
        };
        Some((
            CertifiedBlock::new(block.account(), height, *hash),
            previous,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{Signature, StateBlockArgs};
    fn fixture() -> (ResidualData, Arc<RwLock<ForkCache>>) {
        let forks = Arc::new(RwLock::new(ForkCache::new()));
        (
            ResidualData::new(
                Arc::new(Ledger::new_null()),
                forks.clone(),
                Arc::new(AecService::new_null()),
            ),
            forks,
        )
    }
    #[test]
    fn places_owner_signed_fork_ancestry_without_an_election() {
        let (data, forks) = fixture();
        let parent: Block = StateBlockArgs {
            previous: BlockHash::ZERO,
            ..StateBlockArgs::new_test_instance()
        }
        .into();
        let child: Block = StateBlockArgs {
            previous: parent.hash(),
            ..StateBlockArgs::new_test_instance()
        }
        .into();
        forks.write().unwrap().add(parent.clone());
        forks.write().unwrap().add(child.clone());
        let (placed, previous) = data.placement(&child.hash()).unwrap();
        assert_eq!(placed.height, 2);
        assert_eq!(previous, parent.hash());
        data.retain(ConsensusEpoch::ZERO, [parent.hash(), child.hash()]);
        *forks.write().unwrap() = ForkCache::new();
        assert_eq!(data.placement(&child.hash()).unwrap().0, placed);
        assert_eq!(
            data.retained(ConsensusEpoch::ZERO, &[child.hash()]),
            vec![child]
        );
    }
    #[test]
    fn unknown_parent_or_invalid_owner_signature_has_no_placement() {
        let (data, forks) = fixture();
        let missing: Block = StateBlockArgs {
            previous: BlockHash::from(987),
            ..StateBlockArgs::new_test_instance()
        }
        .into();
        forks.write().unwrap().add(missing.clone());
        assert!(data.placement(&missing.hash()).is_none());
        let mut bad: Block = StateBlockArgs {
            previous: BlockHash::ZERO,
            ..StateBlockArgs::new_test_instance()
        }
        .into();
        bad.set_signature(Signature::new());
        forks.write().unwrap().add(bad.clone());
        assert!(data.placement(&bad.hash()).is_none());
    }
}
