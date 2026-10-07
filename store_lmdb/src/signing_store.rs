use rsnano_nullable_lmdb::{
    DatabaseFlags, LmdbDatabase, LmdbEnvironment, Transaction, WriteFlags, WriteTransaction,
};

use crate::iterator::LmdbIterator;

/// RAI, durable signing records: what a validator signed, persisted before
/// the signature leaves the process, so that a restart never signs what it
/// forbids: its one-shot votes per account slot and epoch, the parent of
/// every block it voted for, the epochs it left, and its frozen reports.
/// The store is a key-value table with prefixed keys; the node encodes the
/// records, this store only keeps them.
pub struct LmdbSigningStore {
    database: LmdbDatabase,
}

impl LmdbSigningStore {
    pub fn new(env: &LmdbEnvironment) -> anyhow::Result<Self> {
        let database = env.create_db(Some("rai_signing"), DatabaseFlags::empty())?;
        Ok(Self { database })
    }

    pub fn database(&self) -> LmdbDatabase {
        self.database
    }

    pub fn put(&self, txn: &mut WriteTransaction, key: &[u8], value: &[u8]) {
        txn.put(self.database, key, value, WriteFlags::empty())
            .unwrap();
    }

    pub fn get(&self, txn: &dyn Transaction, key: &[u8]) -> Option<Vec<u8>> {
        txn.get(self.database, key).ok().map(|value| value.to_vec())
    }

    pub fn delete(&self, txn: &mut WriteTransaction, key: &[u8]) {
        let _ = txn.delete(self.database, key, None);
    }

    pub fn count(&self, txn: &dyn Transaction) -> u64 {
        txn.count(self.database)
    }

    /// Every record whose key starts with the prefix
    pub fn with_prefix(&self, txn: &dyn Transaction, prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let cursor = txn
            .open_ro_cursor(self.database)
            .expect("Could not read the signing store");
        LmdbIterator::new(cursor, |k, v| (k.to_vec(), v.to_vec()))
            .filter(|(key, _)| key.starts_with(prefix))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_kept_by_key_and_listed_by_prefix() {
        let env = LmdbEnvironment::new_null();
        let store = LmdbSigningStore::new(&env).unwrap();
        {
            let mut txn = env.begin_write();
            store.put(&mut txn, b"S1", b"one");
            store.put(&mut txn, b"S2", b"two");
            store.put(&mut txn, b"P1", b"parent");
            txn.commit();
        }
        let txn = env.begin_read();
        assert_eq!(store.get(&txn, b"S1"), Some(b"one".to_vec()));
        assert_eq!(store.get(&txn, b"S9"), None);
        let slots = store.with_prefix(&txn, b"S");
        assert_eq!(slots.len(), 2);
        assert_eq!(store.count(&txn), 3);
    }
}
