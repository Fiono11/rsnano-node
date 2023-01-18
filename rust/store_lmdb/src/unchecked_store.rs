use std::sync::Arc;
use crate::{as_write_txn, count, exists, LmdbEnv, LmdbIteratorImpl};
use lmdb::{Database, DatabaseFlags, WriteFlags};
use rsnano_core::{BlockHash, HashOrAccount, UncheckedInfo, UncheckedKey};
use rsnano_store_traits::{Transaction, UncheckedIterator, UncheckedStore, WriteTransaction};

pub struct LmdbUncheckedStore {
    _env: Arc<LmdbEnv>,
    database: Database,
}

impl LmdbUncheckedStore {
    pub fn new(env: Arc<LmdbEnv>) -> anyhow::Result<Self> {
        let database = env
            .environment
            .create_db(Some("unchecked"), DatabaseFlags::empty())?;
        Ok(Self {
            _env: env,
            database,
        })
    }

    pub fn database(&self) -> Database {
        self.database
    }
}

impl UncheckedStore for LmdbUncheckedStore {
    fn clear(&self, txn: &mut dyn WriteTransaction) {
        as_write_txn(txn).clear_db(self.database).unwrap();
    }

    fn put(
        &self,
        txn: &mut dyn WriteTransaction,
        dependency: &HashOrAccount,
        info: &UncheckedInfo,
    ) {
        let key = UncheckedKey {
            previous: dependency.into(),
            hash: info.block.as_ref().unwrap().read().unwrap().hash(),
        };
        let key_bytes = key.to_bytes();
        let value_bytes = info.to_bytes();
        as_write_txn(txn)
            .put(self.database, &key_bytes, &value_bytes, WriteFlags::empty())
            .unwrap();
    }

    fn exists(&self, txn: &dyn Transaction, key: &UncheckedKey) -> bool {
        exists(txn, self.database, &key.to_bytes())
    }

    fn del(&self, txn: &mut dyn WriteTransaction, key: &UncheckedKey) {
        as_write_txn(txn)
            .del(self.database, &key.to_bytes(), None)
            .unwrap();
    }

    fn begin(&self, txn: &dyn Transaction) -> UncheckedIterator {
        LmdbIteratorImpl::new_iterator(txn, self.database, None, true)
    }

    fn lower_bound(&self, txn: &dyn Transaction, key: &UncheckedKey) -> UncheckedIterator {
        let key_bytes = key.to_bytes();
        LmdbIteratorImpl::new_iterator(txn, self.database, Some(&key_bytes), true)
    }

    fn count(&self, txn: &dyn Transaction) -> u64 {
        count(txn, self.database)
    }

    fn equal_range(&self, txn: &dyn Transaction, dependency: BlockHash) -> (UncheckedIterator, UncheckedIterator) {
        let begin_l = UncheckedKey::new(dependency, BlockHash::zero());
        let end_l = UncheckedKey::new(BlockHash::from(
            dependency.number() + 1), BlockHash::zero());
        let end_iter = if begin_l.previous.number() < end_l.previous.number() {
            self.lower_bound(txn, &end_l) }
        else {
            let mut it = self.begin(txn);
            while !it.is_end() {
                it.next();
            }
            it
        };
            (self.lower_bound (txn, &begin_l), end_iter)
    }

    fn full_range(&self, txn: &dyn Transaction) -> (UncheckedIterator, UncheckedIterator) {
        todo!()
    }
}
