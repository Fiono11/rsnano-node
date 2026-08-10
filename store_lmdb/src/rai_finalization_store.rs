use std::collections::BTreeMap;

use rsnano_nullable_lmdb::{
    DatabaseFlags, Error, LmdbDatabase, LmdbEnvironment, Transaction, WriteFlags, WriteTransaction,
};
use rsnano_types::{Account, BlockHash, ConfirmationHeightInfo, RaiEpoch};

/// Durable RAI metadata. Blocks are absent until certified and finality is
/// immutable. Epoch attribution converges to the earliest valid certificate
/// when evidence arrives out of order. The delta table contains only the last
/// certified frontier for an account in an epoch, avoiding a full frontier
/// snapshot per close.
pub struct LmdbRaiFinalizationStore {
    blocks: LmdbDatabase,
    epoch_deltas: LmdbDatabase,
    baseline_frontiers: LmdbDatabase,
}

impl LmdbRaiFinalizationStore {
    pub fn new(env: &LmdbEnvironment) -> anyhow::Result<Self> {
        Ok(Self {
            blocks: env.create_db(Some("rai_block_finalization_epoch"), DatabaseFlags::empty())?,
            epoch_deltas: env
                .create_db(Some("rai_epoch_frontier_deltas"), DatabaseFlags::empty())?,
            baseline_frontiers: env
                .create_db(Some("rai_baseline_frontiers"), DatabaseFlags::empty())?,
        })
    }

    /// Reclassifies the current cemented ledger as pre-epoch state. Existing
    /// epoch attribution is discarded, while the frontiers remain available
    /// as the base of subsequent close records.
    pub fn reset_to_baseline(
        &self,
        txn: &mut WriteTransaction,
        frontiers: impl IntoIterator<Item = (Account, ConfirmationHeightInfo)>,
    ) {
        self.clear_database(txn, self.blocks);
        self.clear_database(txn, self.epoch_deltas);
        self.clear_database(txn, self.baseline_frontiers);
        for (account, info) in frontiers {
            txn.put(
                self.baseline_frontiers,
                account.as_bytes(),
                &info.to_bytes(),
                WriteFlags::empty(),
            )
            .expect("could not persist RAI baseline frontier");
        }
    }

    fn clear_database(&self, txn: &mut WriteTransaction, database: LmdbDatabase) {
        let keys = {
            let mut cursor = txn
                .open_ro_cursor(database)
                .expect("could not open RAI metadata cursor for reset");
            cursor
                .iter_start()
                .map(|row| {
                    row.expect("could not read RAI metadata during reset")
                        .0
                        .to_vec()
                })
                .collect::<Vec<_>>()
        };
        for key in keys {
            txn.delete(database, &key, None)
                .expect("could not clear RAI metadata during reset");
        }
    }

    pub fn epoch(&self, txn: &dyn Transaction, hash: &BlockHash) -> Option<RaiEpoch> {
        match txn.get(self.blocks, hash.as_bytes()) {
            Ok(bytes) => Some(RaiEpoch::new(u64::from_be_bytes(
                bytes.try_into().expect("invalid RAI finalization epoch"),
            ))),
            Err(Error::NotFound) => None,
            Err(e) => panic!("could not load RAI finalization epoch: {e:?}"),
        }
    }

    fn epoch_account_key(epoch: RaiEpoch, account: &Account) -> Vec<u8> {
        let mut key = Vec::with_capacity(8 + Account::SERIALIZED_SIZE);
        key.extend_from_slice(&epoch.number().to_be_bytes());
        key.extend_from_slice(account.as_bytes());
        key
    }

    /// Assigns the block to its earliest known valid finalization epoch.
    ///
    /// Evidence can arrive out of order when an epoch closes concurrently
    /// with its successor. A later assignment is therefore corrected when an
    /// earlier certificate or certified close is installed, while an earlier
    /// assignment is never moved forward. Returns true when the block is
    /// assigned to `epoch` after the call.
    pub fn put(
        &self,
        txn: &mut WriteTransaction,
        hash: &BlockHash,
        epoch: RaiEpoch,
        account: &Account,
        frontier: &ConfirmationHeightInfo,
    ) -> bool {
        match self.epoch(txn, hash) {
            Some(existing) if existing < epoch => return false,
            Some(existing) if existing > epoch => {
                // If this block was the recorded frontier of the later epoch,
                // remove that stale delta. A later successor, when present,
                // remains the later epoch's frontier and must be preserved.
                if self
                    .frontier_delta(txn, existing, account)
                    .is_some_and(|current| current.frontier == *hash)
                {
                    txn.delete(
                        self.epoch_deltas,
                        &Self::epoch_account_key(existing, account),
                        None,
                    )
                    .expect("could not remove superseded RAI epoch frontier delta");
                }
                txn.put(
                    self.blocks,
                    hash.as_bytes(),
                    &epoch.number().to_be_bytes(),
                    WriteFlags::empty(),
                )
                .expect("could not correct RAI finalization epoch");
            }
            Some(_) => {}
            None => {
                txn.put(
                    self.blocks,
                    hash.as_bytes(),
                    &epoch.number().to_be_bytes(),
                    WriteFlags::NO_OVERWRITE,
                )
                .expect("could not persist RAI finalization epoch");
            }
        }

        if self
            .frontier_delta(txn, epoch, account)
            .is_none_or(|current| frontier.height > current.height)
        {
            txn.put(
                self.epoch_deltas,
                &Self::epoch_account_key(epoch, account),
                &frontier.to_bytes(),
                WriteFlags::empty(),
            )
            .expect("could not persist RAI epoch frontier delta");
        }
        true
    }

    pub fn frontier_delta(
        &self,
        txn: &dyn Transaction,
        epoch: RaiEpoch,
        account: &Account,
    ) -> Option<ConfirmationHeightInfo> {
        let key = Self::epoch_account_key(epoch, account);
        match txn.get(self.epoch_deltas, &key) {
            Ok(mut bytes) => Some(
                ConfirmationHeightInfo::deserialize(&mut bytes)
                    .expect("invalid RAI epoch frontier delta"),
            ),
            Err(Error::NotFound) => None,
            Err(e) => panic!("could not load RAI epoch frontier delta: {e:?}"),
        }
    }

    pub fn counts_by_epoch(&self, txn: &dyn Transaction) -> BTreeMap<RaiEpoch, u64> {
        let mut result = BTreeMap::new();
        let mut cursor = txn
            .open_ro_cursor(self.blocks)
            .expect("could not open RAI finalization cursor");
        for row in cursor.iter_start() {
            let (_, bytes) = row.expect("could not read RAI finalization epoch");
            let epoch = RaiEpoch::new(u64::from_be_bytes(
                bytes.try_into().expect("invalid RAI finalization epoch"),
            ));
            *result.entry(epoch).or_default() += 1;
        }
        result
    }

    pub fn frontiers_through(
        &self,
        txn: &dyn Transaction,
        through: RaiEpoch,
    ) -> BTreeMap<Account, ConfirmationHeightInfo> {
        let mut result: BTreeMap<Account, ConfirmationHeightInfo> = BTreeMap::new();
        let mut baseline = txn
            .open_ro_cursor(self.baseline_frontiers)
            .expect("could not open RAI baseline frontier cursor");
        for row in baseline.iter_start() {
            let (key, mut value) = row.expect("could not read RAI baseline frontier");
            let account = Account::from_bytes(
                key.try_into()
                    .expect("invalid RAI baseline frontier account"),
            );
            let info = ConfirmationHeightInfo::deserialize(&mut value)
                .expect("invalid RAI baseline frontier");
            result.insert(account, info);
        }
        let mut cursor = txn
            .open_ro_cursor(self.epoch_deltas)
            .expect("could not open RAI epoch frontier cursor");
        for row in cursor.iter_start() {
            let (key, mut value) = row.expect("could not read RAI epoch frontier");
            let epoch = RaiEpoch::new(u64::from_be_bytes(
                key[..8].try_into().expect("invalid RAI epoch frontier key"),
            ));
            if epoch > through {
                break;
            }
            let account =
                Account::from_bytes(key[8..].try_into().expect("invalid RAI epoch account key"));
            let info = ConfirmationHeightInfo::deserialize(&mut value)
                .expect("invalid RAI epoch frontier");
            let current = result.entry(account).or_default();
            if info.height > current.height {
                *current = info;
            }
        }
        result
    }

    pub fn frontiers_before(
        &self,
        txn: &dyn Transaction,
        epoch: RaiEpoch,
    ) -> BTreeMap<Account, ConfirmationHeightInfo> {
        let mut result: BTreeMap<Account, ConfirmationHeightInfo> = BTreeMap::new();
        let mut baseline = txn
            .open_ro_cursor(self.baseline_frontiers)
            .expect("could not open RAI baseline frontier cursor");
        for row in baseline.iter_start() {
            let (key, mut value) = row.expect("could not read RAI baseline frontier");
            let account = Account::from_bytes(
                key.try_into()
                    .expect("invalid RAI baseline frontier account"),
            );
            let info = ConfirmationHeightInfo::deserialize(&mut value)
                .expect("invalid RAI baseline frontier");
            result.insert(account, info);
        }
        let mut cursor = txn
            .open_ro_cursor(self.epoch_deltas)
            .expect("could not open RAI epoch frontier cursor");
        for row in cursor.iter_start() {
            let (key, mut value) = row.expect("could not read RAI epoch frontier");
            let stored_epoch = RaiEpoch::new(u64::from_be_bytes(
                key[..8].try_into().expect("invalid RAI epoch frontier key"),
            ));
            if stored_epoch >= epoch {
                break;
            }
            let account =
                Account::from_bytes(key[8..].try_into().expect("invalid RAI epoch account key"));
            let info = ConfirmationHeightInfo::deserialize(&mut value)
                .expect("invalid RAI epoch frontier");
            let current = result.entry(account).or_default();
            if info.height > current.height {
                *current = info;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalization_epoch_moves_only_earlier_and_delta_is_compact() {
        let env = LmdbEnvironment::new_null();
        let store = LmdbRaiFinalizationStore::new(&env).unwrap();
        let hash = BlockHash::from(1);
        let account = Account::from(2);
        let info = ConfirmationHeightInfo::new(3, hash);
        let mut txn = env.begin_write();

        assert!(store.put(&mut txn, &hash, RaiEpoch::new(7), &account, &info));
        assert!(store.put(&mut txn, &hash, RaiEpoch::new(7), &account, &info));
        assert!(!store.put(&mut txn, &hash, RaiEpoch::new(8), &account, &info));
        assert!(store.put(&mut txn, &hash, RaiEpoch::new(6), &account, &info));
        assert_eq!(store.epoch(&txn, &hash), Some(RaiEpoch::new(6)));
        assert_eq!(
            store.frontier_delta(&txn, RaiEpoch::new(6), &account),
            Some(info.clone())
        );
        assert_eq!(store.frontier_delta(&txn, RaiEpoch::new(7), &account), None);
        assert_eq!(store.frontier_delta(&txn, RaiEpoch::new(8), &account), None);
        assert!(store.frontiers_through(&txn, RaiEpoch::new(5)).is_empty());
        assert_eq!(
            store.frontiers_through(&txn, RaiEpoch::new(6)),
            BTreeMap::from([(account, info)])
        );
        assert_eq!(
            store.counts_by_epoch(&txn),
            BTreeMap::from([(RaiEpoch::new(6), 1)])
        );
    }

    #[test]
    fn reset_moves_cemented_frontiers_out_of_epoch_accounting() {
        let env = LmdbEnvironment::new_null();
        let store = LmdbRaiFinalizationStore::new(&env).unwrap();
        let account = Account::from(2);
        let old_hash = BlockHash::from(3);
        let old_info = ConfirmationHeightInfo::new(4, old_hash);
        let mut txn = env.begin_write();
        store.put(&mut txn, &old_hash, RaiEpoch::ZERO, &account, &old_info);

        store.reset_to_baseline(&mut txn, [(account, old_info.clone())]);
        txn.commit();
        let txn = env.begin_read();

        assert!(store.counts_by_epoch(&txn).is_empty());
        assert_eq!(
            store.frontiers_through(&txn, RaiEpoch::ZERO),
            BTreeMap::from([(account, old_info.clone())])
        );
        assert_eq!(
            store.frontiers_before(&txn, RaiEpoch::ZERO),
            BTreeMap::from([(account, old_info)])
        );
        assert_eq!(store.epoch(&txn, &old_hash), None);
    }
}
