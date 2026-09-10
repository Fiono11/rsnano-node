use crate::{AnySet, Ledger};
use rsnano_types::{Blake2HashBuilder, BlockHash};
use std::{collections::BTreeSet, sync::atomic::Ordering};

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
            || epoch <= self.closed_epoch_count.load(Ordering::Acquire)
    }

    pub fn epoch_close_candidate(&self, epoch: u64) -> Vec<BlockHash> {
        let tx = self.store.begin_read();
        self.store
            .consensus_epochs
            .iter(&tx)
            .filter(|(hash, voting_epoch)| {
                *voting_epoch <= epoch || self.store.consensus_epochs.canonical(&tx, hash).is_some()
            })
            .map(|(hash, _)| hash)
            .collect()
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
        let selected: BTreeSet<_> = hashes.iter().copied().collect();
        let tx = self.store.begin_read();
        // A snapshot extends every previously closed snapshot, not arrival-order metadata.
        if self.store.consensus_epochs.iter(&tx).any(|(h, _)| {
            self.store.consensus_epochs.canonical(&tx, &h).is_some() && !selected.contains(&h)
        }) {
            return false;
        }
        let any = self.any();
        hashes.iter().all(|hash| {
            let Some(v) = self.store.consensus_epochs.get(&tx, hash) else {
                return false;
            };
            if v > epoch && self.store.consensus_epochs.canonical(&tx, hash).is_none() {
                return false;
            }
            let Some(block) = any.get_block(hash) else {
                return false;
            };
            block
                .dependent_blocks(&self.constants.epochs, &self.constants.genesis_account)
                .iter()
                .all(|h| h.is_zero() || selected.contains(h))
        })
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
            assert!(!ledger.epoch_application_allowed(1));
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
