use rsnano_nullable_lmdb::{
    DatabaseFlags, Error, LmdbDatabase, LmdbEnvironment, Transaction, WriteFlags, WriteTransaction,
};
use rsnano_types::BlockHash;
/// Local metadata, deliberately outside the block body and block hash.
pub struct ConsensusEpochStore {
    database: LmdbDatabase,
}
impl ConsensusEpochStore {
    pub fn new(env: &LmdbEnvironment) -> anyhow::Result<Self> {
        Ok(Self {
            database: env.create_db(Some("consensus_epochs"), DatabaseFlags::empty())?,
        })
    }
    pub fn iter<'a>(&self, tx: &'a dyn Transaction) -> impl Iterator<Item = (BlockHash, u64)> + 'a {
        crate::LmdbIterator::new(tx.open_ro_cursor(self.database).unwrap(), |key, value| {
            (
                BlockHash::from_slice(key),
                u64::from_le_bytes(value.try_into().unwrap()),
            )
        })
        .filter_map(|(hash, epoch)| hash.map(|hash| (hash, epoch)))
    }
    pub fn get(&self, tx: &dyn Transaction, hash: &BlockHash) -> Option<u64> {
        self.read(tx, hash.as_bytes())
    }
    fn read(&self, tx: &dyn Transaction, key: &[u8]) -> Option<u64> {
        match tx.get(self.database, key) {
            Ok(b) => Some(u64::from_le_bytes(
                b.try_into().expect("invalid consensus epoch"),
            )),
            Err(Error::NotFound) => None,
            Err(e) => panic!("epoch read: {e:?}"),
        }
    }
    pub fn put(&self, tx: &mut WriteTransaction, hash: &BlockHash, epoch: u64) {
        tx.put(
            self.database,
            hash.as_bytes(),
            &epoch.to_le_bytes(),
            WriteFlags::empty(),
        )
        .unwrap();
    }
    pub fn configure_length(&self, tx: &mut WriteTransaction, length: u64) -> anyhow::Result<()> {
        if let Some(previous) = self.read(tx, b"length") {
            anyhow::ensure!(
                previous == length || self.count(tx) == 0,
                "epoch_length cannot change after cementation; use a fresh ledger"
            );
        }
        tx.put(
            self.database,
            b"length",
            &length.to_le_bytes(),
            WriteFlags::empty(),
        )
        .unwrap();
        Ok(())
    }
    pub fn count(&self, tx: &dyn Transaction) -> u64 {
        self.read(tx, b"count").unwrap_or(0)
    }
    pub fn record(&self, tx: &mut WriteTransaction, hash: &BlockHash, epoch: u64) {
        if let Some(previous) = self.get(tx, hash) {
            if epoch < previous {
                self.put(tx, hash, epoch);
            }
            return;
        }
        self.put(tx, hash, epoch);
        let count = self
            .count(tx)
            .checked_add(1)
            .expect("epoch counter overflow");
        tx.put(
            self.database,
            b"count",
            &count.to_le_bytes(),
            WriteFlags::empty(),
        )
        .unwrap();
    }
}
