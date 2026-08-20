use std::{collections::VecDeque, sync::atomic::Ordering};

use rsnano_nullable_lmdb::{Transaction, WriteTransaction};
use rsnano_store_lmdb::LmdbStore;
#[cfg(feature = "rai_protocol")]
use rsnano_types::RaiEpoch;
use rsnano_types::{BlockHash, ConfirmationHeightInfo, SavedBlock};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use crate::LedgerConstants;
#[cfg(feature = "rai_protocol")]
use crate::RepresentativeBlockFinder;

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
        #[cfg(feature = "rai_protocol")] finalization_epoch: Option<RaiEpoch>,
    ) -> (WriteTransaction, Vec<SavedBlock>, Vec<SavedBlock>) {
        let mut result = Vec::new();
        #[allow(unused_mut)]
        let mut newly_finalized = Vec::new();
        #[cfg(feature = "rai_protocol")]
        let finalization_account = finalization_epoch.and_then(|_| {
            self.store
                .block
                .get(&txn, &target_hash)
                .map(|block| block.account())
        });
        #[cfg(feature = "rai_protocol")]
        let preceding_frontier = finalization_epoch.and_then(|epoch| {
            finalization_account.as_ref().and_then(|account| {
                self.store
                    .rai_finalization
                    .frontier_before(&txn, epoch, account)
            })
        });
        let mut stack = VecDeque::new();
        stack.push_back(target_hash);
        while let Some(&hash) = stack.back() {
            let block = self.store.block.get(&txn, &hash).unwrap();

            let dependents =
                block.dependent_blocks(&self.constants.epochs, &self.constants.genesis_account);
            for dependent in dependents.iter() {
                let unconfirmed = !dependent.is_zero() && !self.is_confirmed(&txn, dependent);
                #[cfg(feature = "rai_protocol")]
                let certified_account_predecessor = finalization_epoch.is_some_and(|epoch| {
                    finalization_account == Some(block.account())
                        && !dependent.is_zero()
                        && *dependent == block.previous()
                        && preceding_frontier.as_ref().is_none_or(|base| {
                            self.store
                                .block
                                .get(&txn, dependent)
                                .is_some_and(|predecessor| predecessor.height() > base.height)
                        })
                        && self
                            .store
                            .rai_finalization
                            .epoch(&txn, dependent)
                            .is_none_or(|assigned| assigned > epoch)
                });
                #[cfg(not(feature = "rai_protocol"))]
                let certified_account_predecessor = false;
                if unconfirmed || certified_account_predecessor {
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
                let was_confirmed = self.is_confirmed(&txn, &hash);
                if !was_confirmed {
                    // We must only confirm blocks that have their dependencies confirmed

                    let conf_height = ConfirmationHeightInfo::new(block.height(), block.hash());

                    // Update store
                    self.store
                        .confirmation_height
                        .put(&mut txn, &block.account(), &conf_height);
                    // The persisted weight snapshot seeds the immutable RAI
                    // genesis committee prepared before epoch ticking starts.
                    // Once cementation carries an epoch certificate, close
                    // committees are derived from their exact frontier maps;
                    // rewriting this index for every workload block only adds
                    // LMDB contention to the hot path.
                    #[cfg(feature = "rai_protocol")]
                    if finalization_epoch.is_none() {
                        let representative = block.representative_field().or_else(|| {
                            let rep_hash = RepresentativeBlockFinder::new(&txn, self.store)
                                .find_rep_block(hash);
                            self.store
                                .block
                                .get(&txn, &rep_hash)
                                .and_then(|block| block.representative_field())
                        });
                        let representative =
                            representative.expect("confirmed block must have a representative");
                        self.store
                            .rai_finalization
                            .put_confirmed_account_contribution(
                                &mut txn,
                                &block.account(),
                                representative,
                                block.balance(),
                            );
                    }
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

                    result.push(block.clone());
                }
                #[cfg(feature = "rai_protocol")]
                if let Some(epoch) = finalization_epoch
                    && finalization_account == Some(block.account())
                    && preceding_frontier
                        .as_ref()
                        .is_none_or(|base| block.height() > base.height)
                {
                    let conf_height = ConfirmationHeightInfo::new(block.height(), block.hash());
                    let was_finalized = self
                        .store
                        .rai_finalization
                        .epoch(&txn, &block.hash())
                        .is_some();
                    // Cementation requests can overlap an epoch boundary.
                    // Finality is immutable, while its epoch attribution
                    // converges to the earliest valid certificate: later
                    // requests are inert and earlier evidence repairs an
                    // out-of-order successor-epoch assignment.
                    let _ = self.store.rai_finalization.put(
                        &mut txn,
                        &block.hash(),
                        epoch,
                        &block.account(),
                        &conf_height,
                    );
                    // A close record can be the first RAI finalizer of a block
                    // which ordinary confirmation already cemented. There is
                    // no BlocksConfirmed event in that case, so surface the
                    // first durable finalization assignment separately. Slot
                    // and close paths race through the same store entry; only
                    // the first assignment emits.
                    if was_confirmed && !was_finalized {
                        newly_finalized.push(block.clone());
                    }
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
            if result.len() >= max_blocks {
                break;
            }
        }
        (txn, result, newly_finalized)
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
