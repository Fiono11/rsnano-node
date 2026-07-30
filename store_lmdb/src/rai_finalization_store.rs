use rsnano_nullable_lmdb::{
    DatabaseFlags, Error, LmdbDatabase, LmdbEnvironment, Transaction, WriteFlags, WriteTransaction,
};
use rsnano_types::{Account, BlockHash, ConfirmationHeightInfo, RaiEpoch};

/// Durable RAI metadata. Blocks are absent until certified; certification is
/// immutable. The delta table contains only the last certified frontier for an
/// account in an epoch, avoiding a full frontier snapshot per close.
pub struct LmdbRaiFinalizationStore {
    blocks: LmdbDatabase,
    epoch_deltas: LmdbDatabase,
}

impl LmdbRaiFinalizationStore {
    pub fn new(env: &LmdbEnvironment) -> anyhow::Result<Self> {
        Ok(Self {
            blocks: env.create_db(Some("rai_block_finalization_epoch"), DatabaseFlags::empty())?,
            epoch_deltas: env
                .create_db(Some("rai_epoch_frontier_deltas"), DatabaseFlags::empty())?,
        })
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

    /// Returns false when a different epoch was already assigned.
    pub fn put(
        &self,
        txn: &mut WriteTransaction,
        hash: &BlockHash,
        epoch: RaiEpoch,
        account: &Account,
        frontier: &ConfirmationHeightInfo,
    ) -> bool {
        if let Some(existing) = self.epoch(txn, hash) {
            return existing == epoch;
        }
        txn.put(
            self.blocks,
            hash.as_bytes(),
            &epoch.number().to_be_bytes(),
            WriteFlags::NO_OVERWRITE,
        )
        .expect("could not persist RAI finalization epoch");

        let mut key = Vec::with_capacity(8 + Account::SERIALIZED_SIZE);
        key.extend_from_slice(&epoch.number().to_be_bytes());
        key.extend_from_slice(account.as_bytes());
        txn.put(
            self.epoch_deltas,
            &key,
            &frontier.to_bytes(),
            WriteFlags::empty(),
        )
        .expect("could not persist RAI epoch frontier delta");
        true
    }

    pub fn frontier_delta(
        &self,
        txn: &dyn Transaction,
        epoch: RaiEpoch,
        account: &Account,
    ) -> Option<ConfirmationHeightInfo> {
        let mut key = Vec::with_capacity(8 + Account::SERIALIZED_SIZE);
        key.extend_from_slice(&epoch.number().to_be_bytes());
        key.extend_from_slice(account.as_bytes());
        match txn.get(self.epoch_deltas, &key) {
            Ok(mut bytes) => Some(
                ConfirmationHeightInfo::deserialize(&mut bytes)
                    .expect("invalid RAI epoch frontier delta"),
            ),
            Err(Error::NotFound) => None,
            Err(e) => panic!("could not load RAI epoch frontier delta: {e:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalization_epoch_is_immutable_and_delta_is_compact() {
        let env = LmdbEnvironment::new_null();
        let store = LmdbRaiFinalizationStore::new(&env).unwrap();
        let hash = BlockHash::from(1);
        let account = Account::from(2);
        let info = ConfirmationHeightInfo::new(3, hash);
        let mut txn = env.begin_write();

        assert!(store.put(&mut txn, &hash, RaiEpoch::new(7), &account, &info));
        assert!(store.put(&mut txn, &hash, RaiEpoch::new(7), &account, &info));
        assert!(!store.put(&mut txn, &hash, RaiEpoch::new(8), &account, &info));
        assert_eq!(store.epoch(&txn, &hash), Some(RaiEpoch::new(7)));
        assert_eq!(
            store.frontier_delta(&txn, RaiEpoch::new(7), &account),
            Some(info)
        );
        assert_eq!(store.frontier_delta(&txn, RaiEpoch::new(8), &account), None);
    }
}
