use crate::Ledger;
use rsnano_types::{Blake2HashBuilder, Block, BlockHash};
use std::sync::atomic::Ordering;

/// Prepared, uncommitted close membership. Dropping it aborts the database writes.
/// Callers must serialize finalization checks and commit with certificate application.
#[must_use = "the prepared epoch close must be committed to take effect"]
pub struct PendingEpochClose<'a> {
    ledger: &'a Ledger,
    epoch: u64,
    hashes: &'a [BlockHash],
    transaction: rsnano_nullable_lmdb::WriteTransaction,
}

impl PendingEpochClose<'_> {
    pub fn commit(self) {
        self.transaction.commit();
        self.ledger.publish_epoch_close(self.epoch, self.hashes);
    }
}

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
        if self.canonical_epochs_loaded.load(Ordering::Acquire)
            && self.canonical_epochs.read().unwrap().contains_key(parent)
        {
            return true;
        }
        // An unsealed immediate predecessor may still resolve concurrently.
        // Once sealed, omitted evidence is never a parent for a later proposal.
        let closed = self.closed_epoch_count.load(Ordering::Acquire);
        let candidates = self.epoch_candidates.read().unwrap();
        if (closed..=epoch).any(|e| candidates.contains(&(e, *parent))) {
            return true;
        }
        // Keep the persisted fallback: a concurrent close may have committed
        // after the cache check, or an unsealed predecessor may be cemented only.
        let tx = self.store.begin_read();
        self.store.consensus_epochs.canonical(&tx, parent).is_some()
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
        // Both memberships are sorted. Merge them once while keeping D4 live,
        // rather than searching the entire snapshot for every known candidate.
        let mut snapshot = hashes.iter();
        self.epoch_candidates
            .read()
            .unwrap()
            .range((epoch, BlockHash::ZERO)..=(epoch, BlockHash::MAX))
            .all(|(_, hash)| snapshot.find(|candidate| **candidate >= *hash) == Some(hash))
    }

    pub fn epoch_close_missing_members(&self, epoch: u64, hashes: &[BlockHash]) -> Vec<BlockHash> {
        let candidates = self.epoch_candidates.read().unwrap();
        let tx = self.store.begin_read();
        if hashes.len() < 64 || hashes.windows(2).any(|pair| pair[0] > pair[1]) {
            // Point lookups suit short lists; diagnostics may also supply
            // arbitrary order. Full sorted snapshots use a single merge below.
            return hashes
                .iter()
                .filter(|hash| {
                    !candidates.contains(&(epoch, **hash))
                        && !self.store.consensus_epochs.close_contains(&tx, epoch, hash)
                })
                .copied()
                .collect();
        }
        let mut local = candidates
            .range((epoch, BlockHash::ZERO)..=(epoch, BlockHash::MAX))
            .map(|(_, hash)| hash)
            .peekable();
        hashes
            .iter()
            .filter(|hash| {
                while local.peek().is_some_and(|candidate| *candidate < *hash) {
                    local.next();
                }
                !local.peek().is_some_and(|candidate| *candidate == *hash)
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
        self.prepare_epoch_close(epoch, hashes)?.commit();
        Ok(())
    }

    /// Stage membership writes without publishing canonical state. The node can
    /// do this work before excluding unrelated certificate application, then
    /// assert inclusion of every finalized block immediately before commit.
    pub fn prepare_epoch_close<'a>(
        &'a self,
        epoch: u64,
        hashes: &'a [BlockHash],
    ) -> anyhow::Result<PendingEpochClose<'a>> {
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
        Ok(PendingEpochClose {
            ledger: self,
            epoch,
            hashes,
            transaction: tx,
        })
    }

    fn publish_epoch_close(&self, epoch: u64, hashes: &[BlockHash]) {
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
        self.closed_epoch_count.store(epoch + 1, Ordering::Release);
        self.voting_epoch.fetch_max(epoch + 1, Ordering::AcqRel);
        self.draining_epoch.store(u64::MAX, Ordering::Release);
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

    #[test]
    fn epoch_parents_accept_open_members_and_reject_sealed_omissions() {
        let ledger = Ledger::new_null();
        ledger.configure_epoch_length(25).unwrap();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let included = lattice.genesis().send(1, 1);
        let omitted = lattice.genesis().send(2, 1);
        let next = lattice.genesis().send(3, 1);
        for block in [&included, &omitted] {
            ledger.record_epoch_block(0, block.clone());
            assert!(ledger.epoch_parent_allowed(&block.hash(), 1));
        }
        ledger.record_epoch_block(1, next.clone());
        assert!(!ledger.epoch_parent_allowed(&next.hash(), 0));
        assert!(ledger.epoch_parent_allowed(&next.hash(), 1));
        ledger.close_epoch(0, &[included.hash()]).unwrap();
        assert!(ledger.epoch_parent_allowed(&included.hash(), 1));
        assert!(!ledger.epoch_parent_allowed(&omitted.hash(), 1));
        assert!(ledger.epoch_parent_allowed(&next.hash(), 1));

        // The store remains authoritative while a committed close is being
        // mirrored into memory and for cemented, unsealed predecessors.
        ledger.canonical_epochs.write().unwrap().clear();
        assert!(ledger.epoch_parent_allowed(&included.hash(), 1));
        let cemented = BlockHash::from(27);
        let mut tx = ledger.store.begin_write();
        ledger.store.consensus_epochs.record(&mut tx, &cemented, 1);
        tx.commit();
        assert!(ledger.epoch_parent_allowed(&cemented, 1));
        assert!(!ledger.epoch_parent_allowed(&cemented, 0));
        ledger.close_epoch(1, &[next.hash()]).unwrap();
        assert!(!ledger.epoch_parent_allowed(&cemented, 2));
    }

    use crate::{LedgerBuilder, LedgerConstants, test_helpers::UnsavedBlockLatticeBuilder};
    #[test]
    fn membership_snapshot_does_not_pin_certificate_index() {
        let ledger = Ledger::new_null();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let a = lattice.genesis().send(1, 1);
        let b = lattice.genesis().send(2, 1);
        ledger.record_epoch_block(0, a.clone());
        let snapshot = ledger.epoch_close_candidate(0);
        assert!(ledger.epoch_candidates.try_write().is_ok());
        ledger.record_epoch_block(1, b.clone());
        assert_eq!(snapshot, vec![a.hash()]);
        assert!(ledger.epoch_close_candidate(1).contains(&b.hash()));
        assert!(!ledger.epoch_close_candidate(0).contains(&b.hash()));
        // A late earlier certificate changes future snapshots, never a captured one.
        ledger.record_epoch_block(0, b.clone());
        assert!(ledger.epoch_close_candidate(0).contains(&b.hash()));
        ledger.record_epoch_block(2, b.clone());
        assert!(ledger.epoch_close_candidate(2).contains(&b.hash()));
        assert_eq!(snapshot, vec![a.hash()]);
    }

    #[test]
    fn closing_earlier_epoch_preserves_same_hash_membership_in_overlapping_epoch() {
        let ledger = Ledger::new_null();
        ledger.configure_epoch_length(25).unwrap();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let block = lattice.genesis().send(1, 1);
        let hashes = [block.hash()];
        ledger.record_epoch_block(0, block.clone());
        ledger.record_epoch_block(1, block.clone());
        assert_eq!(ledger.epoch_close_candidate(0), hashes);
        assert_eq!(ledger.epoch_close_candidate(1), hashes);

        // A certified close can omit a locally notarized, nonfinalized fork.
        // Its independently verified membership in the overlapping epoch stays live.
        ledger.close_epoch(0, &[]).unwrap();
        assert!(ledger.epoch_close_candidate(0).is_empty());
        assert_eq!(ledger.epoch_close_candidate(1), hashes);
        assert_eq!(ledger.canonical_confirmation_epoch(&block.hash()), None);
        assert!(ledger.epoch_close_candidate_valid(1, &hashes));
        assert!(ledger.epoch_parent_allowed(&block.hash(), 1));
        assert!(!ledger.epoch_parent_allowed(&block.hash(), 0));

        ledger.record_epoch_block(0, block.clone());
        assert!(ledger.epoch_close_candidate(0).is_empty());
        assert_eq!(ledger.epoch_close_candidate(1), hashes);
        assert_eq!(ledger.epoch_close_missing_members(0, &hashes), hashes);

        ledger.close_epoch(1, &hashes).unwrap();
        assert_eq!(ledger.canonical_confirmation_epoch(&block.hash()), Some(1));
        assert!(ledger.epoch_close_candidate(1).is_empty());
        assert!(ledger.epoch_close_missing_members(1, &hashes).is_empty());
        assert!(ledger.epoch_parent_allowed(&block.hash(), 2));
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
    fn close_membership_checks_follow_live_epoch_candidates() {
        let ledger = Ledger::new_null();
        ledger.epoch_candidates.write().unwrap().extend([
            (0, BlockHash::from(2)),
            (0, BlockHash::from(4)),
            (1, BlockHash::from(3)),
        ]);
        let snapshot: Vec<_> = (1u64..=5).map(BlockHash::from).collect();
        assert!(ledger.epoch_close_contains_known(0, &snapshot));
        assert_eq!(
            ledger.epoch_close_missing_members(0, &snapshot),
            vec![BlockHash::from(1), BlockHash::from(3), BlockHash::from(5)]
        );
        for omitted in [2, 4] {
            let incomplete: Vec<_> = snapshot
                .iter()
                .copied()
                .filter(|hash| *hash != BlockHash::from(omitted))
                .collect();
            assert!(!ledger.epoch_close_contains_known(0, &incomplete));
        }
        // A previously sufficient snapshot must fail as soon as a new local
        // certificate appears, including one beyond the snapshot's last member.
        ledger
            .epoch_candidates
            .write()
            .unwrap()
            .insert((0, BlockHash::from(6)));
        assert!(!ledger.epoch_close_contains_known(0, &snapshot));
        assert!(!ledger.epoch_close_contains_known(0, &[]));
        assert!(ledger.epoch_close_contains_known(2, &[]));
    }

    #[test]
    fn full_snapshot_reconciliation_preserves_missing_member_order() {
        let ledger = Ledger::new_null();
        ledger.epoch_candidates.write().unwrap().extend(
            (0u64..=130)
                .filter(|value| value % 2 == 0)
                .map(|value| (0, BlockHash::from(value))),
        );
        let mut snapshot: Vec<_> = (1u64..=131).map(BlockHash::from).collect();
        let mut missing: Vec<_> = (1u64..=131).step_by(2).map(BlockHash::from).collect();
        assert_eq!(ledger.epoch_close_missing_members(0, &snapshot), missing);
        snapshot.reverse();
        missing.reverse();
        assert_eq!(ledger.epoch_close_missing_members(0, &snapshot), missing);
    }

    #[test]
    fn staged_epoch_close_is_invisible_until_commit_and_aborts_on_drop() {
        let path = std::env::temp_dir().join(format!("rai-close-staged-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(25).unwrap();
            let mut lattice = UnsavedBlockLatticeBuilder::new();
            let a = lattice.genesis().send(1, 1);
            let b = lattice.genesis().send(2, 1);
            ledger.record_epoch_block(0, a.clone());
            let hashes = [a.hash()];
            let pending = ledger.prepare_epoch_close(0, &hashes).unwrap();
            assert!(ledger.closed_epochs().is_empty());
            assert_eq!(ledger.canonical_confirmation_epoch(&a.hash()), None);
            assert!(!ledger.store.consensus_epochs.close_contains(
                &ledger.store.begin_read(),
                0,
                &a.hash()
            ));
            assert_eq!(ledger.epoch_close_candidate(0), hashes);
            drop(pending);
            assert!(ledger.closed_epochs().is_empty());
            assert_eq!(ledger.closed_epoch_count.load(Ordering::Acquire), 0);

            let pending = ledger.prepare_epoch_close(0, &hashes).unwrap();
            // Certificates from the overlapping epoch can still be applied
            // while persistence is staged and must survive its publication.
            ledger.record_epoch_block(1, b.clone());
            pending.commit();
            assert_eq!(ledger.closed_epoch_count.load(Ordering::Acquire), 1);
            assert_eq!(ledger.canonical_confirmation_epoch(&a.hash()), Some(0));
            assert!(ledger.epoch_close_candidate(0).is_empty());
            assert_eq!(ledger.epoch_close_candidate(1), vec![b.hash()]);
            assert_eq!(ledger.closed_epochs().len(), 1);
        }
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn repeated_close_members_keep_their_first_canonical_epoch() {
        let path = std::env::temp_dir().join(format!("rai-close-repeated-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        let mut lattice = UnsavedBlockLatticeBuilder::new();
        let a = lattice.genesis().send(100, 1);
        let b = lattice.genesis().send(101, 1);
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(25).unwrap();
            ledger.record_epoch_block(0, a.clone());
            ledger.close_epoch(0, &[a.hash()]).unwrap();
            ledger.record_epoch_block(1, a.clone());
            ledger.record_epoch_block(1, b.clone());
            let snapshot = ledger.epoch_close_candidate(1);
            ledger.close_epoch(1, &snapshot).unwrap();
            assert!(ledger.epoch_close_missing_members(1, &snapshot).is_empty());
        }
        {
            let ledger = LedgerBuilder::new(path.join("data.ldb"))
                .constants(LedgerConstants::dev())
                .init_thread_count(1)
                .finish()
                .unwrap();
            ledger.configure_epoch_length(25).unwrap();
            assert_eq!(ledger.canonical_confirmation_epoch(&a.hash()), Some(0));
            assert_eq!(ledger.canonical_confirmation_epoch(&b.hash()), Some(1));
            assert!(
                ledger
                    .epoch_close_missing_members(0, &[a.hash()])
                    .is_empty()
            );
            assert_eq!(
                ledger.epoch_close_missing_members(0, &[b.hash()]),
                vec![b.hash()]
            );
            let tx = ledger.store.begin_read();
            assert_eq!(ledger.store.consensus_epochs.closed_blocks(&tx), 2);
            assert_eq!(ledger.closed_epochs().len(), 2);
        }
        std::fs::remove_dir_all(path).unwrap();
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
