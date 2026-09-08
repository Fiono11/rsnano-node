use std::{collections::VecDeque, sync::atomic::Ordering};

use rsnano_nullable_lmdb::{Transaction, WriteTransaction};
use rsnano_store_lmdb::LmdbStore;
use rsnano_types::{BlockHash, ConfirmationHeightInfo, SavedBlock};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use crate::LedgerConstants;

/// Cements Blocks in the ledger
pub(crate) struct BlockCementer<'a> {
    constants: &'a LedgerConstants,
    store: &'a LmdbStore,
    stats: &'a Stats,
}

impl<'a> BlockCementer<'a> {
    pub(crate) fn new(
        store: &'a LmdbStore,
        constants: &'a LedgerConstants,
        stats: &'a Stats,
    ) -> Self {
        Self {
            store,
            constants,
            stats,
        }
    }

    pub(crate) fn confirm(
        &self,
        mut txn: WriteTransaction,
        target_hash: BlockHash,
        max_blocks: usize,
        _epoch: u64,
    ) -> (WriteTransaction, Vec<SavedBlock>) {
        let mut result = Vec::new();
        let mut processed = 0;

        let mut stack = VecDeque::new();
        stack.push_back(target_hash);
        while let Some(&hash) = stack.back() {
            let block = self.store.block.get(&txn, &hash).unwrap();

            let dependents =
                block.dependent_blocks(&self.constants.epochs, &self.constants.genesis_account);
            for dependent in dependents.iter() {
                if !dependent.is_zero() && self.needs_confirmation(&txn, dependent, _epoch) {
                    self.stats.inc(
                        StatType::ConfirmationHeight,
                        DetailType::DependentUnconfirmed,
                    );

                    stack.push_back(*dependent);

                    // Limit the stack size to avoid excessive memory usage
                    // This will forget the bottom of the dependency tree
                    if stack.len() > max_blocks {
                        stack.pop_front();
                    }
                }
            }

            if stack.back() == Some(&hash) {
                stack.pop_back();
                processed += 1;
                #[cfg(feature = "rai_protocol")]
                if self.is_confirmed(&txn, &hash) {
                    if self.store.consensus_epochs.get(&txn, &hash).is_some() {
                        self.store.consensus_epochs.record(&mut txn, &hash, _epoch);
                    } else {
                        self.store.consensus_epochs.put(&mut txn, &hash, 0);
                    }
                }
                if !self.is_confirmed(&txn, &hash) {
                    // We must only confirm blocks that have their dependencies confirmed

                    let conf_height = ConfirmationHeightInfo::new(block.height(), block.hash());

                    #[cfg(feature = "rai_protocol")]
                    self.store.consensus_epochs.record(&mut txn, &hash, _epoch);

                    // Update store
                    self.store
                        .confirmation_height
                        .put(&mut txn, &block.account(), &conf_height);
                    self.store
                        .cache
                        .confirmed_count
                        .fetch_add(1, Ordering::SeqCst);

                    self.stats.add_dir(
                        StatType::ConfirmationHeight,
                        DetailType::BlocksConfirmed,
                        Direction::In,
                        1,
                    );

                    result.push(block);
                }
            } else {
                // Unconfirmed dependencies were added
            }

            // Refresh the transaction to avoid long-running transactions
            // Ensure that the block wasn't rolled back during the refresh

            if txn.is_refresh_needed() {
                txn = self.store.env.refresh(txn);
                if !self.store.block.exists(&txn, &target_hash) {
                    break; // Block was rolled back during cementing
                }
            }

            // Early return might leave parts of the dependency tree unconfirmed
            if processed >= max_blocks {
                break;
            }
        }
        (txn, result)
    }

    fn needs_confirmation(&self, tx: &WriteTransaction, hash: &BlockHash, _epoch: u64) -> bool {
        if !self.is_confirmed(tx, hash) {
            return true;
        }
        #[cfg(feature = "rai_protocol")]
        {
            return self.store.consensus_epochs.get(tx, hash).unwrap_or(0) > _epoch;
        }
        #[cfg(not(feature = "rai_protocol"))]
        false
    }
    fn is_confirmed(&self, tx: &WriteTransaction, hash: &BlockHash) -> bool {
        let Some(block) = self.store.block.get(tx, hash) else {
            return false;
        };
        let Some(info) = self.store.confirmation_height.get(tx, &block.account()) else {
            return false;
        };

        block.height() <= info.height
    }
}
