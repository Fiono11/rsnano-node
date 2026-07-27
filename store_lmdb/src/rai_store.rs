use crate::iterator::LmdbIterator;
use rsnano_nullable_lmdb::{
    DatabaseFlags, Error, LmdbDatabase, LmdbEnvironment, Transaction, WriteFlags, WriteTransaction,
};

/// Stores RAI consensus snapshots as named byte blobs.
pub struct LmdbRaiStore {
    database: LmdbDatabase,
}

impl LmdbRaiStore {
    pub fn new(env: &LmdbEnvironment) -> anyhow::Result<Self> {
        let database = env.create_db(Some("rai"), DatabaseFlags::empty())?;

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
        match txn.get(self.database, key) {
            Ok(bytes) => Some(bytes.to_vec()),
            Err(Error::NotFound) => None,
            Err(e) => panic!("Could not load RAI state {:?}", e),
        }
    }

    pub fn iter<'a>(
        &self,
        txn: &'a dyn Transaction,
    ) -> impl Iterator<Item = (Vec<u8>, Vec<u8>)> + 'a + use<'a> {
        let cursor = txn
            .open_ro_cursor(self.database)
            .expect("Could not read RAI store database");
        LmdbIterator::new(cursor, |key, value| (key.to_vec(), value.to_vec()))
    }

    pub fn del(&self, txn: &mut WriteTransaction, key: &[u8]) {
        txn.delete(self.database, key, None).unwrap();
    }

    pub fn clear(&self, txn: &mut WriteTransaction) {
        txn.clear_db(self.database).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_lmdb::{DeleteEvent, LmdbEnvironment};
    use std::sync::Arc;

    const TEST_DATABASE: LmdbDatabase = LmdbDatabase::new_null(100);

    struct Fixture {
        env: Arc<LmdbEnvironment>,
        store: LmdbRaiStore,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_stored_entries(Vec::new())
        }

        fn with_stored_entries(entries: Vec<(&'static [u8], &'static [u8])>) -> Self {
            let mut env = LmdbEnvironment::null_builder().database("rai", TEST_DATABASE);
            for (key, value) in entries {
                env = env.entry(key, value);
            }
            Self::with_env(env.build().build())
        }

        fn with_env(env: LmdbEnvironment) -> Self {
            let env = Arc::new(env);
            Self {
                store: LmdbRaiStore::new(&env).unwrap(),
                env,
            }
        }
    }

    #[test]
    fn load() {
        let fixture = Fixture::with_stored_entries(vec![(b"close", b"snapshot")]);
        let txn = fixture.env.begin_read();

        assert_eq!(
            fixture.store.get(&txn, b"close"),
            Some(b"snapshot".to_vec())
        );
        assert_eq!(fixture.store.get(&txn, b"missing"), None);
    }

    #[test]
    fn put() {
        let fixture = Fixture::new();
        let mut txn = fixture.env.begin_write();

        fixture.store.put(&mut txn, b"active", b"elections");

        assert_eq!(
            fixture.store.get(&txn, b"active"),
            Some(b"elections".to_vec())
        );
    }

    #[test]
    fn delete() {
        let fixture = Fixture::with_stored_entries(vec![(b"committee", b"snapshot")]);
        let mut txn = fixture.env.begin_write();
        let delete_tracker = txn.track_deletions();

        fixture.store.del(&mut txn, b"committee");

        assert_eq!(
            delete_tracker.output(),
            vec![DeleteEvent {
                key: b"committee".to_vec(),
                database: TEST_DATABASE.into(),
            }]
        )
    }

    #[test]
    fn clear() {
        let fixture = Fixture::new();
        let mut txn = fixture.env.begin_write();
        let clear_tracker = txn.track_clears();

        fixture.store.clear(&mut txn);

        assert_eq!(clear_tracker.output(), vec![TEST_DATABASE.into()]);
    }
}
