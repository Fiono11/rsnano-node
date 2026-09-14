use crate::{AnySet, Ledger};
#[cfg(test)]
use rsnano_types::DependentBlocks;
use rsnano_types::{Blake2HashBuilder, Block, BlockBase, BlockHash};
#[cfg(test)]
use rustc_hash::FxHashMap;
use std::sync::atomic::Ordering;

impl Ledger {
    pub fn closed_epochs(&self) -> std::collections::BTreeMap<u64, BlockHash> {
        let tx = self.store.begin_read();
        (0..self.store.consensus_epochs.closed_count(&tx))
            .filter_map(|e| {
                self.store
                    .consensus_epochs
                    .close_digest(&tx, e)
                    .map(|h| (e, h))
            })
            .collect()
    }
    pub fn canonical_confirmation_epoch(&self, hash: &BlockHash) -> Option<u64> {
        if self.closed_epoch_count.load(Ordering::Acquire) == 0 {
            // Nothing is canonical before the first close.
            return None;
        }
        if self.canonical_epochs_loaded.load(Ordering::Acquire) {
            return self.canonical_epochs.read().unwrap().get(hash).copied();
        }
        self.store
            .consensus_epochs
            .canonical(&self.store.begin_read(), hash)
    }
    pub fn epoch_application_allowed(&self, epoch: u64) -> bool {
        !self.epochs_enabled() || epoch <= self.voting_epoch.load(Ordering::Acquire)
    }

    /// With the fixed single committee, epochs e and e+1 may overlap; e+2
    /// requires the reconstructed close of e. Persisted closes contain membership.
    pub fn epoch_entry_allowed(&self, epoch: u64) -> bool {
        !self.epochs_enabled()
            || (epoch <= self.current_epoch()
                && epoch.saturating_sub(1) <= self.closed_epoch_count.load(Ordering::Acquire))
    }

    pub fn epoch_parent_allowed(&self, parent: &BlockHash, epoch: u64) -> bool {
        if parent.is_zero()
            || *parent == self.constants.genesis_block.hash()
            || !self.epochs_enabled()
        {
            return true;
        }
        let tx = self.store.begin_read();
        if self.store.consensus_epochs.canonical(&tx, parent).is_some() {
            return true;
        }
        // An unsealed immediate predecessor may still resolve concurrently.
        // Once sealed, omitted evidence is never a parent for a later proposal.
        let closed = self.closed_epoch_count.load(Ordering::Acquire);
        let candidates = self.epoch_candidates.read().unwrap();
        (closed..=epoch).any(|e| candidates.contains(&(e, *parent)))
            || self
                .store
                .consensus_epochs
                .get(&tx, parent)
                .is_some_and(|e| e >= closed && e <= epoch)
    }

    /// Record locally verified block-tree membership synchronously with certificate
    /// application, before draining can observe that election as terminated.
    pub fn record_epoch_block(&self, epoch: u64, block: Block) {
        let hash = block.hash();
        if epoch < self.closed_epoch_count.load(Ordering::Acquire) {
            // A late certificate may update an admitted candidate, never resurrect an omission.
            return;
        }
        self.epoch_candidates.write().unwrap().insert((epoch, hash));
        let mut blocks = self.epoch_blocks.write().unwrap();
        if let Some((old, _, _)) = blocks.get_mut(&hash) {
            if epoch < *old {
                *old = epoch;
                self.epoch_metadata
                    .write()
                    .unwrap()
                    .get_mut(&hash)
                    .unwrap()
                    .0 = epoch;
            }
            return;
        }
        let any = self.any();
        let dependencies = match &block {
            Block::LegacySend(b) => b.dependent_blocks(),
            Block::LegacyChange(b) => b.dependent_blocks(),
            Block::LegacyReceive(b) => b.dependent_blocks(),
            Block::LegacyOpen(b) => b.dependent_blocks(&self.constants.genesis_account),
            Block::State(b) => {
                let previous_balance = blocks
                    .get(&b.previous())
                    .and_then(|(_, b, _)| b.balance_field())
                    .or_else(|| any.get_block(&b.previous()).map(|b| b.balance()));
                let receives = b.previous().is_zero()
                    || previous_balance.is_some_and(|balance| b.balance() >= balance);
                let source = if receives && !self.constants.epochs.is_epoch_link(&b.link()) {
                    b.link().into()
                } else {
                    BlockHash::ZERO
                };
                rsnano_types::DependentBlocks::new(b.previous(), source)
            }
        };
        blocks.insert(hash, (epoch, block, dependencies));
        self.epoch_metadata
            .write()
            .unwrap()
            .insert(hash, (epoch, dependencies));
    }

    /// Snapshot the compact, incrementally maintained metadata table. Copying its
    /// contiguous storage avoids traversing block payloads and rehashing every key.
    /// Database scans and dependency closure run after releasing this short lock.
    #[cfg(test)]
    fn epoch_block_metadata(&self) -> FxHashMap<BlockHash, (u64, DependentBlocks)> {
        self.epoch_metadata.read().unwrap().clone()
    }

    /// Only certificate-verified candidates of this epoch, including forks.
    pub fn epoch_close_candidate(&self, epoch: u64) -> Vec<BlockHash> {
        self.epoch_candidates
            .read()
            .unwrap()
            .range((epoch, BlockHash::ZERO)..=(epoch, BlockHash::MAX))
            .map(|(_, hash)| *hash)
            .collect()
    }

    pub fn epoch_state_hash(epoch: u64, hashes: &[BlockHash]) -> BlockHash {
        let mut builder = Blake2HashBuilder::new()
            .update(b"RAI-CLOSE-STATE")
            .update(epoch.to_le_bytes());
        for hash in hashes {
            builder = builder.update(hash.as_bytes());
        }
        builder.build()
    }

    pub fn begin_epoch_drain(&self) -> Option<u64> {
        if !self.epochs_enabled() {
            return None;
        }
        // The epoch clock decides when to drain; block counts do not participate.
        let epoch = self.closed_epoch_count.load(Ordering::Acquire);
        self.draining_epoch.store(epoch, Ordering::Release);
        Some(epoch)
    }

    /// Object validation is independent of D4: an already certified close may
    /// omit a locally known, non-finalizable fork. D4 is enforced before signing.
    pub fn epoch_close_candidate_valid(&self, epoch: u64, hashes: &[BlockHash]) -> bool {
        if hashes.windows(2).any(|w| w[0] >= w[1]) {
            return false;
        }
        self.epoch_close_missing_members(epoch, hashes).is_empty()
    }

    pub fn epoch_close_contains_known(&self, epoch: u64, hashes: &[BlockHash]) -> bool {
        self.epoch_candidates
            .read()
            .unwrap()
            .range((epoch, BlockHash::ZERO)..=(epoch, BlockHash::MAX))
            .all(|(_, h)| hashes.binary_search(h).is_ok())
    }

    pub fn epoch_close_missing_members(&self, epoch: u64, hashes: &[BlockHash]) -> Vec<BlockHash> {
        let candidates = self.epoch_candidates.read().unwrap();
        let tx = self.store.begin_read();
        hashes
            .iter()
            .filter(|hash| {
                !candidates.contains(&(epoch, **hash))
                    && !self.store.consensus_epochs.close_contains(&tx, epoch, hash)
            })
            .copied()
            .collect()
    }

    pub fn epoch_close_candidate_diagnostic(
        &self,
        epoch: u64,
        hashes: &[BlockHash],
    ) -> serde_json::Value {
        serde_json::json!({"missing_objects": self.epoch_close_missing_members(epoch, hashes),
            "contains_known": self.epoch_close_contains_known(epoch, hashes)})
    }

    pub fn close_epoch(&self, epoch: u64, hashes: &[BlockHash]) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.epoch_close_candidate_valid(epoch, hashes),
            "invalid epoch snapshot"
        );
        let mut tx = self.store.begin_write();
        anyhow::ensure!(
            self.store.consensus_epochs.closed_count(&tx) == epoch,
            "out-of-order epoch close"
        );
        let digest = Self::epoch_state_hash(epoch, hashes);
        self.store
            .consensus_epochs
            .close(&mut tx, epoch, digest, hashes);
        tx.commit();
        {
            // Mirror the persisted mapping: a hash keeps the epoch of its first close.
            let mut canonical = self.canonical_epochs.write().unwrap();
            for hash in hashes {
                canonical.entry(*hash).or_insert(epoch);
            }
        }
        self.epoch_candidates
            .write()
            .unwrap()
            .retain(|(e, _)| *e != epoch);
        let mut blocks = self.epoch_blocks.write().unwrap();
        blocks.retain(|hash, (e, _, _)| *e != epoch || hashes.binary_search(hash).is_ok());
        self.epoch_metadata
            .write()
            .unwrap()
            .retain(|hash, (e, _)| *e != epoch || hashes.binary_search(hash).is_ok());
        self.closed_epoch_count.store(epoch + 1, Ordering::Release);
        self.voting_epoch.fetch_max(epoch + 1, Ordering::AcqRel);
        self.draining_epoch.store(u64::MAX, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_bind_epoch_and_membership_but_not_finalization() {
        let ledger = Ledger::new_null();
        let block: Block = rsnano_types::SavedBlock::new_test_instance().into();
        ledger.record_epoch_block(7, block.clone());
        let before = ledger.epoch_close_candidate(7);
        let root = Ledger::epoch_state_hash(7, &before);
        ledger.record_epoch_block(7, block.clone());
        assert_eq!(
            Ledger::epoch_state_hash(7, &ledger.epoch_close_candidate(7)),
            root
        );
        assert_ne!(Ledger::epoch_state_hash(8, &before), root);
        assert!(ledger.epoch_close_candidate(8).is_empty());
        ledger.record_epoch_block(8, block);
        assert_eq!(ledger.epoch_close_candidate(8), before);
    }

    #[test]
    fn close_discards_omitted_candidates_and_fences_late_membership() {
        let ledger = Ledger::new_null();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let a = lattice.genesis().send(1, 1);
        let b = lattice.genesis().send(2, 1);
        ledger.record_epoch_block(0, a.clone());
        ledger.record_epoch_block(0, b.clone());
        ledger.close_epoch(0, &[a.hash()]).unwrap();
        assert_eq!(ledger.canonical_confirmation_epoch(&a.hash()), Some(0));
        assert_eq!(ledger.canonical_confirmation_epoch(&b.hash()), None);
        assert!(ledger.epoch_close_candidate(1).is_empty());
        ledger.record_epoch_block(0, b.clone());
        assert!(ledger.epoch_close_candidate(0).is_empty());
        assert_eq!(
            ledger.epoch_close_missing_members(0, &[a.hash(), b.hash()]),
            vec![b.hash()]
        );
    }

    #[test]
    fn canonical_epochs_are_served_from_memory_after_loading() {
        let ledger = Ledger::new_null();
        ledger.configure_epoch_length(10).unwrap();
        assert!(ledger.canonical_epochs_loaded.load(Ordering::Acquire));
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let a = lattice.genesis().send(1, 1);
        let b = lattice.genesis().send(2, 1);
        ledger.record_epoch_block(0, a.clone());
        assert_eq!(ledger.canonical_confirmation_epoch(&a.hash()), None);
        ledger.close_epoch(0, &[a.hash()]).unwrap();
        assert_eq!(ledger.canonical_confirmation_epoch(&a.hash()), Some(0));
        assert_eq!(ledger.canonical_confirmation_epoch(&b.hash()), None);
        assert_eq!(ledger.canonical_epochs.read().unwrap().len(), 1);
    }

    use crate::{LedgerBuilder, LedgerConstants, test_helpers::UnsavedBlockLatticeBuilder};
    #[test]
    fn membership_metadata_snapshot_does_not_pin_certificate_index() {
        let ledger = Ledger::new_null();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let a = lattice.genesis().send(1, 1);
        let b = lattice.genesis().send(2, 1);
        ledger.record_epoch_block(0, a.clone());
        let snapshot = ledger.epoch_block_metadata();
        assert!(ledger.epoch_blocks.try_write().is_ok());
        assert!(ledger.epoch_metadata.try_write().is_ok());
        ledger.record_epoch_block(1, b.clone());
        assert!(!snapshot.contains_key(&b.hash()));
        assert_eq!(snapshot[&a.hash()].0, 0);
        assert!(ledger.epoch_close_candidate(1).contains(&b.hash()));
        assert!(!ledger.epoch_close_candidate(0).contains(&b.hash()));
        // A late earlier certificate changes future snapshots, never a captured one.
        ledger.record_epoch_block(0, b.clone());
        assert!(ledger.epoch_close_candidate(0).contains(&b.hash()));
        assert_eq!(ledger.epoch_block_metadata()[&b.hash()].0, 0);
        ledger.record_epoch_block(2, b.clone());
        assert_eq!(ledger.epoch_block_metadata()[&b.hash()].0, 0);
        assert!(!snapshot.contains_key(&b.hash()));
    }

    #[test]
    fn missing_members_are_snapshot_hashes_without_local_epoch_membership() {
        let ledger = Ledger::new_null();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let a = lattice.genesis().send(1, 1);
        let b = lattice.genesis().send(2, 1);
        ledger.record_epoch_block(0, a.clone());
        ledger.record_epoch_block(1, b.clone());
        let peer = vec![a.hash(), b.hash(), BlockHash::from(7)];
        assert_eq!(
            ledger.epoch_close_missing_members(0, &peer),
            vec![b.hash(), BlockHash::from(7)]
        );
        assert_eq!(
            ledger.epoch_close_missing_members(1, &peer),
            vec![a.hash(), BlockHash::from(7)]
        );
    }

    #[test]
    fn close_snapshot_retains_only_epoch_candidates_including_notarized_forks() {
        use rsnano_types::{DEV_GENESIS_KEY, StateBlockArgs};
        let path = std::env::temp_dir().join(format!("rai-close-forks-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(25).unwrap();
            let mut lattice = UnsavedBlockLatticeBuilder::new();
            let a = lattice.genesis().send(100, 1);
            ledger.process_one(&a).unwrap();
            ledger.confirm(a.hash());
            ledger.record_epoch_block(0, a.clone());
            let b = lattice.genesis().send(101, 1);
            ledger.process_one(&b).unwrap();
            let fork: Block = StateBlockArgs {
                key: &DEV_GENESIS_KEY,
                previous: b.previous(),
                representative: 999.into(),
                balance: b.balance_field().unwrap(),
                link: b.link_field().unwrap(),
                work: 0.into(),
            }
            .into();
            assert_eq!(b.qualified_root(), fork.qualified_root());
            // Payload eligibility is supplied only after certificate verification.
            ledger.record_epoch_block(0, b.clone());
            ledger.record_epoch_block(0, fork.clone());
            let snapshot = ledger.epoch_close_candidate(0);
            for hash in [a.hash(), b.hash(), fork.hash()] {
                assert!(snapshot.contains(&hash));
            }
            assert!(ledger.epoch_close_candidate_valid(0, &snapshot));
            let without_parent: Vec<_> = snapshot
                .iter()
                .copied()
                .filter(|h| *h != a.hash())
                .collect();
            assert!(ledger.epoch_close_candidate_valid(0, &without_parent));
            assert!(!ledger.epoch_close_contains_known(0, &without_parent));
            ledger.close_epoch(0, &snapshot).unwrap();
            assert_eq!(ledger.canonical_confirmation_epoch(&fork.hash()), Some(0));
            assert!(ledger.epoch_close_candidate(1).is_empty());
        }
        // Canonical fork membership survives even without an account-ledger payload.
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(25).unwrap();
            let snapshot = ledger.epoch_close_candidate(1);
            assert!(snapshot.is_empty());
            assert!(ledger.epoch_close_candidate_valid(1, &snapshot));
            ledger.close_epoch(1, &snapshot).unwrap();
        }
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn epoch_closure_freezes_membership_and_releases_buffered_epoch() {
        let path = std::env::temp_dir().join(format!("rai-close-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(25).unwrap();
            let mut lattice = UnsavedBlockLatticeBuilder::new();
            let a = lattice.genesis().send(100, 1);
            let b = lattice.genesis().send(101, 1);
            ledger.process_one(&a).unwrap();
            ledger.process_one(&b).unwrap();
            ledger.confirm(a.hash());
            ledger.record_epoch_block(0, a.clone());
            assert_eq!(
                ledger.current_epoch(),
                0,
                "Counting alone does not advance voting"
            );
            assert_eq!(ledger.canonical_confirmation_epoch(&a.hash()), None);
            assert_eq!(ledger.begin_epoch_drain(), Some(0));
            ledger.voting_epoch.store(1, Ordering::Release);
            assert!(ledger.epoch_application_allowed(1));
            assert!(!ledger.epoch_application_allowed(2));
            let snapshot = ledger.epoch_close_candidate(0);
            assert!(ledger.close_epoch(1, &snapshot).is_err());
            ledger.close_epoch(0, &snapshot).unwrap();
            assert!(ledger.epoch_application_allowed(1));
            assert_eq!(ledger.canonical_confirmation_epoch(&a.hash()), Some(0));
            ledger.confirm(b.hash());
            ledger.record_epoch_block(1, b.clone());
            assert_eq!(ledger.canonical_confirmation_epoch(&b.hash()), None);
            assert_eq!(ledger.begin_epoch_drain(), Some(1));
            let next = ledger.epoch_close_candidate(1);
            ledger.close_epoch(1, &next).unwrap();
            assert_eq!(ledger.canonical_confirmation_epoch(&a.hash()), Some(0));
            assert_eq!(ledger.canonical_confirmation_epoch(&b.hash()), Some(1));
            assert_eq!(ledger.closed_epochs().len(), 2);
        }
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(25).unwrap();
            assert_eq!(ledger.current_epoch(), 2);
            assert_eq!(ledger.closed_epochs().len(), 2);
        }
        std::fs::remove_dir_all(path).unwrap();
    }
}
