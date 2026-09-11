use crate::{AnySet, Ledger};
use rsnano_types::{Blake2HashBuilder, Block, BlockBase, BlockHash, DependentBlocks};
use rustc_hash::{FxHashMap, FxHashSet};
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
        self.store
            .consensus_epochs
            .canonical(&self.store.begin_read(), hash)
    }
    pub fn epoch_application_allowed(&self, epoch: u64) -> bool {
        self.epoch_length.load(Ordering::Relaxed) == 0
            || epoch <= self.voting_epoch.load(Ordering::Acquire)
    }

    /// Record locally verified block-tree membership synchronously with certificate
    /// application, before draining can observe that election as terminated.
    pub fn record_epoch_block(&self, epoch: u64, block: Block) {
        let hash = block.hash();
        let mut blocks = self.epoch_blocks.write().unwrap();
        if let Some((old, _, _)) = blocks.get_mut(&hash) {
            if epoch < *old {
                *old = epoch;
                self.epoch_metadata.write().unwrap().get_mut(&hash).unwrap().0 = epoch;
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
        self.epoch_metadata.write().unwrap().insert(hash, (epoch, dependencies));
    }

    /// Snapshot the compact, incrementally maintained metadata table. Copying its
    /// contiguous storage avoids traversing block payloads and rehashing every key.
    /// Database scans and dependency closure run after releasing this short lock.
    fn epoch_block_metadata(&self) -> FxHashMap<BlockHash, (u64, DependentBlocks)> {
        self.epoch_metadata.read().unwrap().clone()
    }

    pub fn epoch_close_candidate(&self, epoch: u64) -> Vec<BlockHash> {
        let blocks = self.epoch_block_metadata();
        let tx = self.store.begin_read();
        let mut selected: FxHashSet<_> =
            self.store.consensus_epochs.canonical_hashes(&tx).collect();
        selected.extend(
            self.store
                .consensus_epochs
                .iter(&tx)
                .filter(|(_, e)| *e <= epoch)
                .map(|(h, _)| h),
        );
        selected.extend(
            blocks
                .iter()
                .filter(|(_, (e, _))| *e <= epoch)
                .map(|(h, _)| *h),
        );
        let any = self.any();
        let mut pending: Vec<_> = selected.iter().copied().collect();
        while let Some(hash) = pending.pop() {
            let dependencies = blocks
                .get(&hash)
                .map(|(_, d)| *d)
                .or_else(|| {
                    any.get_block(&hash).map(|b| {
                        b.dependent_blocks(&self.constants.epochs, &self.constants.genesis_account)
                    })
                })
                .unwrap_or_default();
            for dependency in dependencies.iter().copied() {
                if !dependency.is_zero() && selected.insert(dependency) {
                    pending.push(dependency);
                }
            }
        }
        let mut hashes: Vec<_> = selected.into_iter().collect();
        hashes.sort_unstable();
        hashes
    }

    pub fn epoch_state_hash(hashes: &[BlockHash]) -> BlockHash {
        let mut builder = Blake2HashBuilder::new().update(b"rai-epoch-state-v1");
        for hash in hashes {
            builder = builder.update(hash.as_bytes());
        }
        builder.build()
    }

    pub fn begin_epoch_drain(&self) -> Option<u64> {
        if self.epoch_length.load(Ordering::Relaxed) == 0 {
            return None;
        }
        // The epoch clock decides when to drain; block counts do not participate.
        let epoch = self.closed_epoch_count.load(Ordering::Acquire);
        self.draining_epoch.store(epoch, Ordering::Release);
        Some(epoch)
    }

    pub fn epoch_close_candidate_valid(&self, epoch: u64, hashes: &[BlockHash]) -> bool {
        if hashes.is_empty() || hashes.windows(2).any(|w| w[0] >= w[1]) {
            return false;
        }
        let selected: FxHashSet<_> = hashes.iter().copied().collect();
        let blocks = self.epoch_block_metadata();
        let tx = self.store.begin_read();
        let canonical: FxHashSet<_> = self.store.consensus_epochs.canonical_hashes(&tx).collect();
        if !canonical.is_subset(&selected) {
            return false;
        }
        let any = self.any();
        hashes.iter().all(|hash| {
            if canonical.contains(hash) {
                return true;
            }
            if let Some((e, dependencies)) = blocks.get(hash) {
                return *e <= epoch && dependencies.iter().all(|h| selected.contains(h));
            }
            // Cemented dependencies and setup blocks remain eligible independently
            // of whether their election is still retained in the block tree.
            let Some(v) = self.store.consensus_epochs.get(&tx, hash) else {
                return false;
            };
            if v > epoch {
                return false;
            }
            any.get_block(hash).is_some_and(|block| {
                block
                    .dependent_blocks(&self.constants.epochs, &self.constants.genesis_account)
                    .iter()
                    .all(|h| h.is_zero() || selected.contains(h))
            })
        })
    }

    /// Opt-in diagnostics for a complete snapshot rejected by this node.
    pub fn epoch_close_candidate_diagnostic(
        &self,
        epoch: u64,
        hashes: &[BlockHash],
    ) -> serde_json::Value {
        let selected: FxHashSet<_> = hashes.iter().copied().collect();
        let blocks = self.epoch_block_metadata();
        let tx = self.store.begin_read();
        if let Some(hash) = self
            .store
            .consensus_epochs
            .canonical_hashes(&tx)
            .find(|h| !selected.contains(h))
        {
            return serde_json::json!({"reason":"missing_prior_member","hash":hash});
        }
        let any = self.any();
        for hash in hashes {
            if self.store.consensus_epochs.canonical(&tx, hash).is_some() {
                continue;
            }
            if let Some((e, dependencies)) = blocks.get(hash) {
                if *e > epoch {
                    return serde_json::json!({"reason":"later_notarization","hash":hash,"local_epoch":e});
                }
                if let Some(missing) = dependencies.iter().find(|h| !selected.contains(h)) {
                    return serde_json::json!({"reason":"missing_dependency","hash":hash,"dependency":missing});
                }
            } else {
                let metadata = self.store.consensus_epochs.get(&tx, hash);
                if metadata.is_none_or(|e| e > epoch) {
                    return serde_json::json!({"reason":"no_eligible_local_membership","hash":hash,"cemented_epoch":metadata,"in_account_ledger":any.get_block(hash).is_some()});
                }
                let Some(block) = any.get_block(hash) else {
                    return serde_json::json!({"reason":"missing_cemented_payload","hash":hash});
                };
                if let Some(missing) = block
                    .dependent_blocks(&self.constants.epochs, &self.constants.genesis_account)
                    .iter()
                    .find(|h| !h.is_zero() && !selected.contains(h))
                {
                    return serde_json::json!({"reason":"missing_cemented_dependency","hash":hash,"dependency":missing});
                }
            }
        }
        serde_json::json!({"reason":"ledger_valid_check_parent_or_round"})
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
        let digest = Self::epoch_state_hash(hashes);
        self.store
            .consensus_epochs
            .close(&mut tx, epoch, digest, hashes);
        tx.commit();
        self.closed_epoch_count.store(epoch + 1, Ordering::Release);
        self.voting_epoch.fetch_max(epoch + 1, Ordering::AcqRel);
        self.draining_epoch.store(u64::MAX, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn close_snapshot_retains_uncemented_notarized_forks_and_dependencies() {
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
            assert!(!ledger.epoch_close_candidate_valid(0, &without_parent));
            ledger.close_epoch(0, &snapshot).unwrap();
            assert_eq!(ledger.canonical_confirmation_epoch(&fork.hash()), Some(0));
            assert_eq!(ledger.epoch_close_candidate(1), snapshot);
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
            assert_eq!(snapshot.len(), 4);
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
