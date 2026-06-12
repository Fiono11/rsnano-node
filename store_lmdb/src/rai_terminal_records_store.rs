use crate::{LmdbIterator, RAI_TERMINAL_RECORDS_TEST_DATABASE};
use rsnano_nullable_lmdb::{
    ConfiguredDatabase, DatabaseFlags, Error, LmdbDatabase, LmdbEnvironment, Transaction,
    WriteFlags, WriteTransaction,
};

pub struct LmdbRaiTerminalRecordsStore {
    database: LmdbDatabase,
}

impl LmdbRaiTerminalRecordsStore {
    pub fn new(env: &LmdbEnvironment) -> anyhow::Result<Self> {
        let database = env.create_db(Some("rai_terminal_records"), DatabaseFlags::empty())?;

        Ok(Self { database })
    }

    pub fn database(&self) -> LmdbDatabase {
        self.database
    }

    pub fn put(&self, txn: &mut WriteTransaction, key: &[u8], value: &[u8]) {
        txn.put(self.database, key, value, WriteFlags::empty())
            .expect("This should never fail");
    }

    pub fn get(&self, tx: &dyn Transaction, key: &[u8]) -> Option<Vec<u8>> {
        let result = tx.get(self.database, key);
        match result {
            Err(Error::NotFound) => None,
            Ok(bytes) => Some(bytes.to_vec()),
            Err(e) => panic!("Could not load Rai terminal record {:?}", e),
        }
    }

    pub fn del(&self, tx: &mut WriteTransaction, key: &[u8]) {
        tx.delete(self.database, key, None).unwrap();
    }

    pub fn iter<'tx>(
        &self,
        tx: &'tx dyn Transaction,
    ) -> impl Iterator<Item = (Vec<u8>, Vec<u8>)> + 'tx + use<'tx> {
        let cursor = tx.open_ro_cursor(self.database).unwrap();
        LmdbIterator::new(cursor, read_record)
    }
}

fn read_record(key: &[u8], value: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (key.to_vec(), value.to_vec())
}

pub struct ConfiguredRaiTerminalRecordsDatabaseBuilder {
    database: ConfiguredDatabase,
}

impl ConfiguredRaiTerminalRecordsDatabaseBuilder {
    pub fn new() -> Self {
        Self {
            database: ConfiguredDatabase::new(
                RAI_TERMINAL_RECORDS_TEST_DATABASE,
                "rai_terminal_records",
            ),
        }
    }

    pub fn record(mut self, key: Vec<u8>, value: Vec<u8>) -> Self {
        self.database.insert(key, value);
        self
    }

    pub fn build(self) -> ConfiguredDatabase {
        self.database
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_lmdb::{DeleteEvent, PutEvent};
    use std::sync::Arc;

    const TEST_DATABASE: LmdbDatabase = LmdbDatabase::new_null(100);

    struct Fixture {
        env: Arc<LmdbEnvironment>,
        store: LmdbRaiTerminalRecordsStore,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_stored_entries(Vec::new())
        }

        fn with_stored_entries(entries: Vec<(Vec<u8>, Vec<u8>)>) -> Self {
            let mut env =
                LmdbEnvironment::null_builder().database("rai_terminal_records", TEST_DATABASE);
            for (key, value) in entries {
                env = env.entry(&key, &value);
            }
            Self::with_env(env.build().build())
        }

        fn with_env(env: LmdbEnvironment) -> Self {
            let env = Arc::new(env);
            Self {
                store: LmdbRaiTerminalRecordsStore::new(&env).unwrap(),
                env,
            }
        }
    }

    #[test]
    fn put() {
        let fixture = Fixture::new();
        let mut txn = fixture.env.begin_write();
        let put_tracker = txn.track_puts();

        fixture.store.put(&mut txn, &[1, 2, 3], &[4, 5, 6]);

        assert_eq!(
            put_tracker.output(),
            vec![PutEvent {
                database: TEST_DATABASE,
                key: vec![1, 2, 3],
                value: vec![4, 5, 6],
                flags: WriteFlags::empty()
            }]
        );
    }

    #[test]
    fn get() {
        let fixture = Fixture::with_stored_entries(vec![(vec![1, 2, 3], vec![4, 5, 6])]);
        let txn = fixture.env.begin_read();

        assert_eq!(fixture.store.get(&txn, &[1, 2, 3]), Some(vec![4, 5, 6]));
        assert_eq!(fixture.store.get(&txn, &[7, 8, 9]), None);
    }

    #[test]
    fn delete() {
        let fixture = Fixture::with_stored_entries(vec![(vec![1, 2, 3], vec![4, 5, 6])]);
        let mut txn = fixture.env.begin_write();
        let delete_tracker = txn.track_deletions();

        fixture.store.del(&mut txn, &[1, 2, 3]);

        assert_eq!(
            delete_tracker.output(),
            vec![DeleteEvent {
                key: vec![1, 2, 3],
                database: TEST_DATABASE.into(),
            }]
        )
    }
}
