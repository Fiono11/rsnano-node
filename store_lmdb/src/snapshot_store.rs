use crate::{LmdbIterator, SNAPSHOT_TEST_DATABASE};
use rsnano_nullable_lmdb::{
    ConfiguredDatabase, DatabaseFlags, Error, LmdbDatabase, LmdbEnvironment, Transaction,
    WriteFlags, WriteTransaction,
};
use rsnano_types::{Account, BlockHash, Blake2Hash, DeserializationError, SnapshotNumber, read_u32_be};
use std::io::{Read, Write};

/// Special key for storing the current snapshot number
const CURRENT_SNAPSHOT_NUMBER_KEY: &[u8] = b"current_snapshot_number";

/// Data stored for each snapshot
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotData {
    /// The winning proposal hash, or None if nil-confirmed
    pub proposal_hash: Option<Blake2Hash>,
    /// The frontiers that were agreed upon
    pub frontiers: Vec<(Account, BlockHash)>,
}

impl SnapshotData {
    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        // Write a flag: 0 = nil, 1 = has proposal hash
        if let Some(hash) = &self.proposal_hash {
            writer.write_all(&[1])?;
            hash.serialize(writer)?;
        } else {
            writer.write_all(&[0])?;
        }

        // Write frontiers count
        writer.write_all(&(self.frontiers.len() as u32).to_be_bytes())?;

        // Write each frontier
        for (account, hash) in &self.frontiers {
            account.serialize(writer)?;
            hash.serialize(writer)?;
        }

        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        // Read flag
        let mut flag = [0u8; 1];
        reader.read_exact(&mut flag).map_err(|_| DeserializationError::InvalidData)?;
        let proposal_hash = if flag[0] == 1 {
            Some(Blake2Hash::deserialize(reader).map_err(|_| DeserializationError::InvalidData)?)
        } else {
            None
        };

        // Read frontiers count
        let mut count_bytes = [0u8; 4];
        reader.read_exact(&mut count_bytes).map_err(|_| DeserializationError::InvalidData)?;
        let count = u32::from_be_bytes(count_bytes);

        // Read frontiers
        let mut frontiers = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let account = Account::deserialize(reader)?;
            let hash = BlockHash::deserialize(reader).map_err(|_| DeserializationError::InvalidData)?;
            frontiers.push((account, hash));
        }

        Ok(Self {
            proposal_hash,
            frontiers,
        })
    }
}

/// Store for ledger snapshots
pub struct LmdbSnapshotStore {
    database: LmdbDatabase,
}

impl LmdbSnapshotStore {
    pub fn new(env: &LmdbEnvironment) -> anyhow::Result<Self> {
        let database = env.create_db(Some("snapshots"), DatabaseFlags::empty())?;
        Ok(Self { database })
    }

    pub fn database(&self) -> LmdbDatabase {
        self.database
    }

    /// Store a snapshot
    pub fn put(
        &self,
        txn: &mut WriteTransaction,
        snapshot_number: SnapshotNumber,
        data: &SnapshotData,
    ) {
        let mut buffer = Vec::new();
        data.serialize(&mut buffer)
            .expect("Should serialize snapshot data");
        txn.put(
            self.database,
            &snapshot_number.to_be_bytes(),
            &buffer,
            WriteFlags::empty(),
        )
        .expect("Should store snapshot");
    }

    /// Get a snapshot by snapshot number
    pub fn get(
        &self,
        tx: &dyn Transaction,
        snapshot_number: SnapshotNumber,
    ) -> Option<SnapshotData> {
        let result = tx.get(self.database, &snapshot_number.to_be_bytes());
        match result {
            Err(Error::NotFound) => None,
            Ok(mut bytes) => {
                SnapshotData::deserialize(&mut bytes)
                    .ok()
                    .or_else(|| {
                        panic!("Could not deserialize snapshot {}", snapshot_number);
                    })
            }
            Err(e) => panic!("Could not load snapshot {}: {:?}", snapshot_number, e),
        }
    }

    /// Get the current snapshot number
    pub fn get_current_snapshot_number(&self, tx: &dyn Transaction) -> SnapshotNumber {
        let result = tx.get(self.database, CURRENT_SNAPSHOT_NUMBER_KEY);
        match result {
            Err(Error::NotFound) => 0, // Default to 0 if not found
            Ok(mut bytes) => {
                read_u32_be(&mut bytes).unwrap_or_else(|_| {
                    panic!("Could not deserialize current snapshot number");
                })
            }
            Err(e) => panic!("Could not load current snapshot number: {:?}", e),
        }
    }

    /// Set the current snapshot number
    pub fn set_current_snapshot_number(
        &self,
        txn: &mut WriteTransaction,
        snapshot_number: SnapshotNumber,
    ) {
        txn.put(
            self.database,
            CURRENT_SNAPSHOT_NUMBER_KEY,
            &snapshot_number.to_be_bytes(),
            WriteFlags::empty(),
        )
        .expect("Should store current snapshot number");
    }

    /// Iterate over all snapshots
    pub fn iter<'tx>(
        &self,
        tx: &'tx dyn Transaction,
    ) -> impl Iterator<Item = (SnapshotNumber, SnapshotData)> + 'tx + use<'tx> {
        let cursor = tx.open_ro_cursor(self.database).unwrap();
        LmdbIterator::new(cursor, read_snapshot_record)
    }
}

fn read_snapshot_record(mut key: &[u8], mut value: &[u8]) -> (SnapshotNumber, SnapshotData) {
    // Skip the special key for current snapshot number
    if key == CURRENT_SNAPSHOT_NUMBER_KEY {
        // This shouldn't happen in normal iteration, but handle it gracefully
        let snapshot_number = read_u32_be(&mut key).unwrap_or(0);
        let data = SnapshotData::deserialize(&mut value).unwrap_or_else(|_| {
            SnapshotData {
                proposal_hash: None,
                frontiers: Vec::new(),
            }
        });
        return (snapshot_number, data);
    }

    let snapshot_number = read_u32_be(&mut key).unwrap();
    let data = SnapshotData::deserialize(&mut value)
        .unwrap_or_else(|_| panic!("Could not deserialize snapshot {}", snapshot_number));
    (snapshot_number, data)
}

pub struct ConfiguredSnapshotDatabaseBuilder {
    database: ConfiguredDatabase,
}

impl ConfiguredSnapshotDatabaseBuilder {
    pub fn new() -> Self {
        Self {
            database: ConfiguredDatabase::new(SNAPSHOT_TEST_DATABASE, "snapshots"),
        }
    }

    pub fn snapshot(
        mut self,
        snapshot_number: SnapshotNumber,
        data: &SnapshotData,
    ) -> Self {
        let mut buffer = Vec::new();
        data.serialize(&mut buffer).expect("Should serialize");
        self.database
            .insert(&snapshot_number.to_be_bytes(), buffer);
        self
    }

    pub fn current_snapshot_number(mut self, snapshot_number: SnapshotNumber) -> Self {
        self.database
            .insert(CURRENT_SNAPSHOT_NUMBER_KEY, &snapshot_number.to_be_bytes());
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
    use rsnano_types::{Blake2Hash, PrivateKey};
    use std::sync::Arc;

    const TEST_DATABASE: LmdbDatabase = LmdbDatabase::new_null(100);

    struct Fixture {
        env: Arc<LmdbEnvironment>,
        store: LmdbSnapshotStore,
    }

    impl Fixture {
        fn new() -> Self {
            let env = Arc::new(LmdbEnvironment::new_null());
            Self {
                store: LmdbSnapshotStore::new(&env).unwrap(),
                env,
            }
        }
    }

    #[test]
    fn put_and_get_snapshot() {
        let fixture = Fixture::new();
        let snapshot_number = 5;
        let data = SnapshotData {
            proposal_hash: Some(Blake2Hash::from(123)),
            frontiers: vec![
                (Account::from(1), BlockHash::from(100)),
                (Account::from(2), BlockHash::from(200)),
            ],
        };

        let mut txn = fixture.env.begin_write();
        fixture.store.put(&mut txn, snapshot_number, &data);
        txn.commit();

        let txn = fixture.env.begin_read();
        let retrieved = fixture.store.get(&txn, snapshot_number).unwrap();

        assert_eq!(retrieved, data);
    }

    #[test]
    fn put_and_get_nil_snapshot() {
        let fixture = Fixture::new();
        let snapshot_number = 10;
        let data = SnapshotData {
            proposal_hash: None,
            frontiers: vec![],
        };

        let mut txn = fixture.env.begin_write();
        fixture.store.put(&mut txn, snapshot_number, &data);
        txn.commit();

        let txn = fixture.env.begin_read();
        let retrieved = fixture.store.get(&txn, snapshot_number).unwrap();

        assert_eq!(retrieved, data);
    }

    #[test]
    fn get_current_snapshot_number_defaults_to_zero() {
        let fixture = Fixture::new();
        let txn = fixture.env.begin_read();
        assert_eq!(fixture.store.get_current_snapshot_number(&txn), 0);
    }

    #[test]
    fn set_and_get_current_snapshot_number() {
        let fixture = Fixture::new();
        let snapshot_number = 42;

        let mut txn = fixture.env.begin_write();
        fixture.store.set_current_snapshot_number(&mut txn, snapshot_number);
        txn.commit();

        let txn = fixture.env.begin_read();
        assert_eq!(
            fixture.store.get_current_snapshot_number(&txn),
            snapshot_number
        );
    }

    #[test]
    fn serialize_and_deserialize_snapshot_data() {
        let data = SnapshotData {
            proposal_hash: Some(Blake2Hash::from(456)),
            frontiers: vec![
                (Account::from(10), BlockHash::from(1000)),
                (Account::from(20), BlockHash::from(2000)),
            ],
        };

        let mut buffer = Vec::new();
        data.serialize(&mut buffer).unwrap();

        let deserialized = SnapshotData::deserialize(&mut buffer.as_slice()).unwrap();
        assert_eq!(deserialized, data);
    }

    #[test]
    fn serialize_and_deserialize_nil_snapshot_data() {
        let data = SnapshotData {
            proposal_hash: None,
            frontiers: vec![],
        };

        let mut buffer = Vec::new();
        data.serialize(&mut buffer).unwrap();

        let deserialized = SnapshotData::deserialize(&mut buffer.as_slice()).unwrap();
        assert_eq!(deserialized, data);
    }
}
