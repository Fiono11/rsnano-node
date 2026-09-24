use crate::consensus::{AecService, ForkCache, election::CertifiedBlock};
use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_types::{Block, BlockBase, BlockHash, ConsensusEpoch};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
    received: Mutex<crate::consensus::bounded_hash_map::BoundedHashMap<BlockHash, Block>>,
}
impl ResidualData {
    pub fn new(ledger: Arc<Ledger>, forks: Arc<RwLock<ForkCache>>, aec: Arc<AecService>) -> Self {
        Self {
            ledger,
            forks,
            aec,
            retained: Mutex::new(BTreeMap::new()),
            received: Mutex::new(crate::consensus::bounded_hash_map::BoundedHashMap::new(
                65_536,
            )),
        }
    }
    /// Network ingress has checked work; only owner-signed state payloads enter
    /// this cache. Ledger admission is independent and may await predecessors.
    pub fn receive(&self, block: &Block) -> bool {
        let hash = block.hash();
        let mut received = self.received.lock().unwrap();
        if received.contains_key(&hash) {
            return true;
        }
        let Block::State(state) = block else {
            return false;
        };
        if state.verify_signature().is_err() {
            return false;
        }
        received.insert(hash, block.clone());
        true
    }
    /// Diagnostic: why this node holds no own first vote for a block
    /// others first-voted
    pub fn unvoted_reason(
        &self,
        hash: &BlockHash,
        active: bool,
        checkpoint: Option<&crate::consensus::election::EpochLedger>,
    ) -> &'static str {
        let any = self.ledger.any();
        let Some(block) = any.get_block(hash) else {
            return if self.block(hash).is_some() {
                "not_in_ledger_held_as_evidence"
            } else {
                "not_in_ledger"
            };
        };
        if any.confirmed().block_exists(hash) {
            return "cemented_here";
        }
        if active {
            return "active_not_voted";
        }
        match crate::consensus::unattached_dependency(&any, &block, checkpoint) {
            Some(crate::consensus::Unattached::Previous) => "previous_not_final",
            Some(crate::consensus::Unattached::Link) => "source_not_final",
            Some(crate::consensus::Unattached::Block) => "block_missing",
            None => "attachable_not_active",
        }
    }

    pub fn ledger_holds(&self, hash: &BlockHash) -> bool {
        self.ledger.any().block_exists(hash)
    }

    fn block(&self, hash: &BlockHash) -> Option<Block> {
        if let Some(block) = self.received.lock().unwrap().get(hash).cloned() {
            return Some(block);
        }
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
        let mut seen = HashSet::new();
        for hash in hashes {
            let mut current = hash;
            for _ in 0..256 {
                if current.is_zero() || !seen.insert(current) {
                    break;
                }
                let known = self
                    .retained
                    .lock()
                    .unwrap()
                    .get(&epoch)
                    .and_then(|e| e.get(&current))
                    .cloned();
                let Some(block) = known.or_else(|| self.block(&current)) else {
                    break;
                };
                let previous = block.previous();
                self.retained
                    .lock()
                    .unwrap()
                    .entry(epoch)
                    .or_default()
                    .entry(current)
                    .or_insert(block);
                current = previous;
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
    /// The retained blocks of these hashes with the ancestry a peer needs to
    /// place them, down to the first block cemented here: a cemented block
    /// is final and every correct peer obtains it through the ledger, so
    /// re-gossiping it only costs every receiver a signature check
    pub fn with_ancestry(&self, epoch: ConsensusEpoch, hashes: &[BlockHash]) -> Vec<Block> {
        let retained = self.retained.lock().unwrap();
        let Some(blocks) = retained.get(&epoch) else {
            return Vec::new();
        };
        let confirmed = self.ledger.confirmed();
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for hash in hashes {
            let mut current = *hash;
            let mut chain = Vec::new();
            for _ in 0..256 {
                if current.is_zero() || !seen.insert(current) || confirmed.block_exists(&current) {
                    break;
                }
                let Some(block) = blocks.get(&current) else {
                    break;
                };
                chain.push(block.clone());
                current = block.previous();
            }
            result.extend(chain.into_iter().rev());
        }
        result
    }
    /// A block to hand to a requester as evidence, from wherever it is held
    pub fn evidence_block(&self, hash: &BlockHash) -> Option<Block> {
        self.block(hash)
    }

    /// The block to fetch before `hash` can be placed: `hash` itself when
    /// it is not held, otherwise the first ancestor that is not. None once
    /// it can be placed, or when its chain can not be placed at all.
    pub fn missing_ancestor(&self, hash: &BlockHash) -> Option<BlockHash> {
        let mut current = *hash;
        for _ in 0..256 {
            if self.ledger.any().block_exists(&current) {
                return None;
            }
            let Some(Block::State(block)) = self.block(&current) else {
                return Some(current);
            };
            let previous = block.previous();
            if previous.is_zero() {
                return None;
            }
            current = previous;
        }
        None
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
    fn evidence_waiting_for_a_parent_can_be_placed_without_ledger_admission() {
        let (data, _) = fixture();
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
        assert!(data.receive(&child));
        assert!(data.placement(&child.hash()).is_none());
        assert!(data.receive(&parent));
        assert_eq!(data.placement(&child.hash()).unwrap().0.height, 2);
        assert!(data.ledger.any().get_block(&child.hash()).is_none());
        data.retain(ConsensusEpoch::ZERO, [child.hash()]);
        assert_eq!(
            data.with_ancestry(ConsensusEpoch::ZERO, &[child.hash()]),
            vec![parent, child]
        );
        let mut bad: Block = StateBlockArgs {
            previous: BlockHash::from(555),
            ..StateBlockArgs::new_test_instance()
        }
        .into();
        bad.set_signature(Signature::new());
        assert!(!data.receive(&bad));
    }

    #[test]
    fn the_first_block_to_fetch_is_the_deepest_one_missing() {
        let (data, _) = fixture();
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
        assert_eq!(data.missing_ancestor(&child.hash()), Some(child.hash()));
        assert!(data.receive(&child));
        assert_eq!(data.missing_ancestor(&child.hash()), Some(parent.hash()));
        assert!(data.receive(&parent));
        assert_eq!(data.missing_ancestor(&child.hash()), None);
        assert_eq!(data.evidence_block(&parent.hash()), Some(parent));
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
