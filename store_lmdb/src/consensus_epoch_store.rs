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
                value.try_into().map(u64::from_le_bytes).unwrap_or(0),
            )
        })
        .filter_map(|(hash, epoch)| hash.map(|hash| (hash, epoch)))
    }
    pub fn closed_count(&self, tx: &dyn Transaction) -> u64 {
        self.read(tx, b"closed_count").unwrap_or(0)
    }
    pub fn closed_blocks(&self, tx: &dyn Transaction) -> u64 {
        self.read(tx, b"closed_blocks").unwrap_or(0)
    }
    pub fn canonical(&self, tx: &dyn Transaction, hash: &BlockHash) -> Option<u64> {
        let mut key = vec![b'C'];
        key.extend_from_slice(hash.as_bytes());
        self.read(tx, &key)
    }
    pub fn close(
        &self,
        tx: &mut WriteTransaction,
        epoch: u64,
        digest: BlockHash,
        hashes: &[BlockHash],
    ) {
        assert_eq!(epoch, self.closed_count(tx));
        for hash in hashes {
            if self.canonical(tx, hash).is_none() {
                let mut key = vec![b'C'];
                key.extend_from_slice(hash.as_bytes());
                tx.put(
                    self.database,
                    &key,
                    &epoch.to_le_bytes(),
                    WriteFlags::empty(),
                )
                .unwrap();
            }
        }
        tx.put(
            self.database,
            b"closed_blocks",
            &(hashes.len() as u64).to_le_bytes(),
            WriteFlags::empty(),
        )
        .unwrap();
        let mut key = b"closed_hash".to_vec();
        key.extend_from_slice(&epoch.to_le_bytes());
        tx.put(self.database, &key, digest.as_bytes(), WriteFlags::empty())
            .unwrap();
        tx.put(
            self.database,
            b"closed_count",
            &(epoch + 1).to_le_bytes(),
            WriteFlags::empty(),
        )
        .unwrap();
    }
    pub fn close_digest(&self, tx: &dyn Transaction, epoch: u64) -> Option<BlockHash> {
        let mut key = b"closed_hash".to_vec();
        key.extend_from_slice(&epoch.to_le_bytes());
        tx.get(self.database, &key)
            .ok()
            .and_then(BlockHash::from_slice)
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
