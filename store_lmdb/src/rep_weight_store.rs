use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rsnano_nullable_lmdb::{
    ConfiguredDatabase, DatabaseFlags, LmdbDatabase, LmdbEnvironment, RoCursor, Transaction,
    WriteFlags, WriteTransaction,
    sys::{MDB_FIRST, MDB_NEXT, MDB_cursor_op},
};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{Amount, Epoch, PublicKey};

use crate::REP_WEIGHT_TEST_DATABASE;

pub struct LmdbRepWeightStore {
    database: LmdbDatabase,
    delete_listener: OutputListenerMt<PublicKey>,
    put_listener: OutputListenerMt<(PublicKey, Amount)>,
}

impl LmdbRepWeightStore {
    pub fn new(env: &LmdbEnvironment) -> anyhow::Result<Self> {
        Self::with_database_name(env, "rep_weights")
    }

    pub fn with_database_name(env: &LmdbEnvironment, db_name: &str) -> anyhow::Result<Self> {
        let database = env.create_db(Some(db_name), DatabaseFlags::empty())?;

        Ok(Self {
            database,
            delete_listener: OutputListenerMt::new(),
            put_listener: OutputListenerMt::new(),
        })
    }

    pub fn track_deletions(&self) -> Arc<OutputTrackerMt<PublicKey>> {
        self.delete_listener.track()
    }

    pub fn track_puts(&self) -> Arc<OutputTrackerMt<(PublicKey, Amount)>> {
        self.put_listener.track()
    }

    pub fn database(&self) -> LmdbDatabase {
        self.database
    }

    pub fn get(&self, txn: &dyn Transaction, pub_key: &PublicKey) -> Option<Amount> {
        match txn.get(self.database, pub_key.as_bytes()) {
            Ok(mut bytes) => Some(Amount::deserialize(&mut bytes).expect("Should be valid amount")),
            Err(rsnano_nullable_lmdb::Error::NotFound) => None,
            Err(e) => {
                panic!("Could not load rep_weight: {:?}", e);
            }
        }
    }

    pub fn put(&self, txn: &mut WriteTransaction, representative: PublicKey, weight: Amount) {
        self.put_listener.emit((representative, weight));

        txn.put(
            self.database,
            representative.as_bytes(),
            &weight.to_be_bytes(),
            WriteFlags::empty(),
        )
        .unwrap();
    }

    pub fn del(&self, txn: &mut WriteTransaction, representative: &PublicKey) {
        self.delete_listener.emit(*representative);

        txn.delete(self.database, representative.as_bytes(), None)
            .unwrap();
    }

    pub fn count(&self, txn: &dyn Transaction) -> u64 {
        txn.count(self.database)
    }

    pub fn iter<'a>(&self, txn: &'a dyn Transaction) -> RepWeightIterator<'a> {
        let cursor = txn.open_ro_cursor(self.database).unwrap();
        RepWeightIterator {
            cursor,
            operation: MDB_FIRST,
        }
    }
}

pub struct RepWeightIterator<'txn> {
    cursor: RoCursor<'txn>,
    operation: MDB_cursor_op,
}

impl<'txn> Iterator for RepWeightIterator<'txn> {
    type Item = (PublicKey, Amount);

    fn next(&mut self) -> Option<Self::Item> {
        match self.cursor.get(None, None, self.operation) {
            Err(rsnano_nullable_lmdb::Error::NotFound) => None,
            Ok((Some(k), v)) => {
                self.operation = MDB_NEXT;
                Some((
                    PublicKey::from_slice(k).unwrap(),
                    Amount::from_be_bytes(v.try_into().unwrap()),
                ))
            }
            Ok(_) => unreachable!(),
            Err(_) => unreachable!(),
        }
    }
}

/// Manages multiple rep weight stores, one per epoch.
/// Each epoch has its own LMDB database (e.g., "rep_weights_epoch_0", "rep_weights_epoch_1").
pub struct EpochRepWeightStore {
    env: Arc<LmdbEnvironment>,
    stores: Arc<Mutex<HashMap<Epoch, Arc<LmdbRepWeightStore>>>>,
    dummy_stores: Arc<Mutex<HashMap<Epoch, Arc<LmdbRepWeightStore>>>>,
}

impl EpochRepWeightStore {
    pub fn new(env: Arc<LmdbEnvironment>) -> Self {
        Self {
            env,
            stores: Arc::new(Mutex::new(HashMap::new())),
            dummy_stores: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn get_or_create_store(&self, epoch: Epoch) -> anyhow::Result<Arc<LmdbRepWeightStore>> {
        let mut stores = self.stores.lock().unwrap();

        if let Some(store) = stores.get(&epoch) {
            return Ok(Arc::clone(store));
        }

        // Create new store for this epoch
        let db_name = format!("rep_weights_epoch_{}", epoch as u8);
        let store = Arc::new(LmdbRepWeightStore::with_database_name(&self.env, &db_name)?);

        stores.insert(epoch, Arc::clone(&store));
        Ok(store)
    }

    fn get_or_create_dummy_store(&self, epoch: Epoch) -> anyhow::Result<Arc<LmdbRepWeightStore>> {
        let mut stores = self.dummy_stores.lock().unwrap();

        if let Some(store) = stores.get(&epoch) {
            return Ok(Arc::clone(store));
        }

        // Create new dummy store for this epoch
        let db_name = format!("rep_weights_dummy_epoch_{}", epoch as u8);
        let store = Arc::new(LmdbRepWeightStore::with_database_name(&self.env, &db_name)?);

        stores.insert(epoch, Arc::clone(&store));
        Ok(store)
    }

    pub fn get(
        &self,
        txn: &dyn Transaction,
        epoch: Epoch,
        pub_key: &PublicKey,
    ) -> anyhow::Result<Option<Amount>> {
        let store = self.get_or_create_store(epoch)?;
        Ok(store.get(txn, pub_key))
    }

    pub fn put(
        &self,
        txn: &mut WriteTransaction,
        epoch: Epoch,
        representative: PublicKey,
        weight: Amount,
    ) -> anyhow::Result<()> {
        let store = self.get_or_create_store(epoch)?;
        store.put(txn, representative, weight);
        Ok(())
    }

    pub fn del(
        &self,
        txn: &mut WriteTransaction,
        epoch: Epoch,
        representative: &PublicKey,
    ) -> anyhow::Result<()> {
        let store = self.get_or_create_store(epoch)?;
        store.del(txn, representative);
        Ok(())
    }

    pub fn count(&self, txn: &dyn Transaction, epoch: Epoch) -> anyhow::Result<u64> {
        let store = self.get_or_create_store(epoch)?;
        Ok(store.count(txn))
    }

    pub fn iter<'a>(
        &self,
        txn: &'a dyn Transaction,
        epoch: Epoch,
    ) -> anyhow::Result<RepWeightIterator<'a>> {
        let store = self.get_or_create_store(epoch)?;
        Ok(store.iter(txn))
    }

    pub fn track_deletions(&self, epoch: Epoch) -> anyhow::Result<Arc<OutputTrackerMt<PublicKey>>> {
        let store = self.get_or_create_store(epoch)?;
        Ok(store.track_deletions())
    }

    pub fn track_puts(
        &self,
        epoch: Epoch,
    ) -> anyhow::Result<Arc<OutputTrackerMt<(PublicKey, Amount)>>> {
        let store = self.get_or_create_store(epoch)?;
        Ok(store.track_puts())
    }

    /// Get all epochs that have been initialized
    pub fn initialized_epochs(&self) -> Vec<Epoch> {
        let stores = self.stores.lock().unwrap();
        stores.keys().copied().collect()
    }

    /// Get the previous epoch, or None if already at the minimum epoch
    fn previous_epoch(&self, epoch: Epoch) -> Option<Epoch> {
        match epoch {
            Epoch::Epoch2 => Some(Epoch::Epoch1),
            Epoch::Epoch1 => Some(Epoch::Epoch0),
            Epoch::Epoch0 => Some(Epoch::Unspecified),
            Epoch::Unspecified | Epoch::Invalid => None,
        }
    }

    /// Get the effective rep weight for a representative, following the fallback logic:
    /// 1. Check regular weights for the given epoch
    /// 2. If not found, check if dummy weights exist (if so, fallback to previous epoch)
    /// 3. If no weights exist, also fallback to previous epoch
    /// 4. Continue until finding regular weights or reaching the minimum epoch
    ///
    /// Returns Some(weight) if regular weights are found, None if no weights exist after fallback
    pub fn get_effective_weight(
        &self,
        txn: &dyn Transaction,
        start_epoch: Epoch,
        pub_key: &PublicKey,
    ) -> anyhow::Result<Option<Amount>> {
        let mut epoch = start_epoch;

        loop {
            // First, check for regular weights
            if let Some(weight) = self.get(txn, epoch, pub_key)? {
                return Ok(Some(weight));
            }

            // No regular weights found, fallback to previous epoch
            // (Dummy weights may exist, but we still fallback to find regular weights)

            // Try previous epoch
            if let Some(prev_epoch) = self.previous_epoch(epoch) {
                epoch = prev_epoch;
                continue;
            } else {
                // Reached minimum epoch, no weights found
                return Ok(None);
            }
        }
    }

    // Dummy rep weight methods - these store rep weights that nodes agree on during consensus
    // but don't actually change the state. They're stored per epoch for consensus tracking.

    pub fn put_dummy(
        &self,
        txn: &mut WriteTransaction,
        epoch: Epoch,
        representative: PublicKey,
        weight: Amount,
    ) -> anyhow::Result<()> {
        let store = self.get_or_create_dummy_store(epoch)?;
        store.put(txn, representative, weight);
        Ok(())
    }

    pub fn get_dummy(
        &self,
        txn: &dyn Transaction,
        epoch: Epoch,
        pub_key: &PublicKey,
    ) -> anyhow::Result<Option<Amount>> {
        let store = self.get_or_create_dummy_store(epoch)?;
        Ok(store.get(txn, pub_key))
    }

    pub fn del_dummy(
        &self,
        txn: &mut WriteTransaction,
        epoch: Epoch,
        representative: &PublicKey,
    ) -> anyhow::Result<()> {
        let store = self.get_or_create_dummy_store(epoch)?;
        store.del(txn, representative);
        Ok(())
    }

    pub fn count_dummy(&self, txn: &dyn Transaction, epoch: Epoch) -> anyhow::Result<u64> {
        let store = self.get_or_create_dummy_store(epoch)?;
        Ok(store.count(txn))
    }

    pub fn iter_dummy<'a>(
        &self,
        txn: &'a dyn Transaction,
        epoch: Epoch,
    ) -> anyhow::Result<RepWeightIterator<'a>> {
        let store = self.get_or_create_dummy_store(epoch)?;
        Ok(store.iter(txn))
    }

    pub fn track_dummy_deletions(
        &self,
        epoch: Epoch,
    ) -> anyhow::Result<Arc<OutputTrackerMt<PublicKey>>> {
        let store = self.get_or_create_dummy_store(epoch)?;
        Ok(store.track_deletions())
    }

    pub fn track_dummy_puts(
        &self,
        epoch: Epoch,
    ) -> anyhow::Result<Arc<OutputTrackerMt<(PublicKey, Amount)>>> {
        let store = self.get_or_create_dummy_store(epoch)?;
        Ok(store.track_puts())
    }

    /// Get all epochs that have dummy rep weights initialized
    pub fn initialized_dummy_epochs(&self) -> Vec<Epoch> {
        let stores = self.dummy_stores.lock().unwrap();
        stores.keys().copied().collect()
    }

    /// Get all epochs that have regular (non-dummy) rep weights with at least one entry
    pub fn epochs_with_regular_weights(&self, txn: &dyn Transaction) -> anyhow::Result<Vec<Epoch>> {
        let mut epochs_with_weights = Vec::new();

        // First, check stores that are already in the HashMap
        {
            let stores = self.stores.lock().unwrap();
            for (&epoch, store) in stores.iter() {
                let count = store.count(txn);
                if count > 0 {
                    epochs_with_weights.push(epoch);
                }
            }
        }

        // Also check all possible epochs to discover databases that exist but aren't in the HashMap yet
        let all_epochs = [Epoch::Epoch0, Epoch::Epoch1, Epoch::Epoch2];

        for epoch in all_epochs {
            // Skip if we already found this epoch
            if epochs_with_weights.contains(&epoch) {
                continue;
            }

            // Try to open the database for this epoch (without creating it)
            let db_name = format!("rep_weights_epoch_{}", epoch as u8);
            match self.env.open_db(Some(&db_name)) {
                Ok(database) => {
                    // Database exists, check if it has any entries
                    let count = txn.count(database);
                    if count > 0 {
                        epochs_with_weights.push(epoch);
                    }
                }
                Err(_) => {
                    // Database doesn't exist, skip it
                }
            }
        }

        // Sort by epoch value (Epoch0 < Epoch1 < Epoch2)
        epochs_with_weights.sort();
        Ok(epochs_with_weights)
    }

    /// Clean up old epochs, keeping only the last two epochs with regular rep weights.
    /// This only cleans regular (non-dummy) rep weights; dummy weights are preserved.
    pub fn cleanup_old_epochs(&self, txn: &mut WriteTransaction) -> anyhow::Result<Vec<Epoch>> {
        let epochs_with_weights = self.epochs_with_regular_weights(txn)?;

        if epochs_with_weights.len() <= 2 {
            // Already have 2 or fewer epochs, nothing to clean
            return Ok(Vec::new());
        }

        // Keep only the last two epochs
        let epochs_to_keep: Vec<Epoch> =
            epochs_with_weights.iter().rev().take(2).copied().collect();
        let epochs_to_remove: Vec<Epoch> = epochs_with_weights
            .into_iter()
            .filter(|epoch| !epochs_to_keep.contains(epoch))
            .collect();

        // Remove old epochs from the stores HashMap and clear their databases
        let mut stores = self.stores.lock().unwrap();
        for epoch in &epochs_to_remove {
            if let Some(store) = stores.remove(epoch) {
                // Clear the database
                txn.clear_db(store.database())?;
            }
        }

        Ok(epochs_to_remove)
    }
}

pub struct ConfiguredRepWeightDatabaseBuilder {
    database: ConfiguredDatabase,
}

impl ConfiguredRepWeightDatabaseBuilder {
    pub fn new() -> Self {
        Self {
            database: ConfiguredDatabase::new(REP_WEIGHT_TEST_DATABASE, "rep_weights"),
        }
    }

    pub fn entry(mut self, account: PublicKey, weight: Amount) -> Self {
        self.database
            .insert(account.as_bytes(), weight.to_be_bytes());
        self
    }

    pub fn build(self) -> ConfiguredDatabase {
        self.database
    }

    pub fn create(hashes: Vec<(PublicKey, Amount)>) -> ConfiguredDatabase {
        let mut builder = Self::new();
        for (account, weight) in hashes {
            builder = builder.entry(account, weight);
        }
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_lmdb::{DeleteEvent, PutEvent, WriteFlags};

    #[test]
    fn count() {
        let fixture =
            Fixture::with_stored_data(vec![(1.into(), 100.into()), (2.into(), 200.into())]);
        let txn = fixture.env.begin_read();

        assert_eq!(fixture.store.count(&txn), 2);
    }

    #[test]
    fn put() {
        let fixture = Fixture::new();
        let mut txn = fixture.env.begin_write();
        let put_tracker = txn.track_puts();
        let account = PublicKey::from(1);
        let weight = Amount::from(42);

        fixture.store.put(&mut txn, account, weight);

        assert_eq!(
            put_tracker.output(),
            vec![PutEvent {
                database: REP_WEIGHT_TEST_DATABASE.into(),
                key: account.as_bytes().to_vec(),
                value: weight.to_be_bytes().to_vec(),
                flags: WriteFlags::empty()
            }]
        );
    }

    #[test]
    fn load_weight() {
        let account = PublicKey::from(1);
        let weight = Amount::from(42);
        let fixture = Fixture::with_stored_data(vec![(account, weight)]);
        let txn = fixture.env.begin_read();

        let result = fixture.store.get(&txn, &account);

        assert_eq!(result, Some(weight));
    }

    #[test]
    fn delete() {
        let fixture = Fixture::new();
        let mut txn = fixture.env.begin_write();
        let delete_tracker = txn.track_deletions();
        let account = PublicKey::from(1);

        fixture.store.del(&mut txn, &account);

        assert_eq!(
            delete_tracker.output(),
            vec![DeleteEvent {
                database: REP_WEIGHT_TEST_DATABASE.into(),
                key: account.as_bytes().to_vec()
            }]
        )
    }

    #[test]
    fn iter_empty() {
        let fixture = Fixture::new();
        let txn = fixture.env.begin_read();
        let mut iter = fixture.store.iter(&txn);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn iter() {
        let account1 = PublicKey::from(1);
        let account2 = PublicKey::from(2);
        let weight1 = Amount::from(100);
        let weight2 = Amount::from(200);
        let fixture = Fixture::with_stored_data(vec![(account1, weight1), (account2, weight2)]);

        let txn = fixture.env.begin_read();
        let mut iter = fixture.store.iter(&txn);
        assert_eq!(iter.next(), Some((account1, weight1)));
        assert_eq!(iter.next(), Some((account2, weight2)));
        assert_eq!(iter.next(), None);
    }

    struct Fixture {
        env: Arc<LmdbEnvironment>,
        store: LmdbRepWeightStore,
    }

    impl Fixture {
        pub fn new() -> Self {
            Self::with_stored_data(Vec::new())
        }

        pub fn with_stored_data(entries: Vec<(PublicKey, Amount)>) -> Self {
            let env = LmdbEnvironment::null_builder()
                .configured_database(ConfiguredRepWeightDatabaseBuilder::create(entries))
                .build();
            let env = Arc::new(env);
            Self {
                store: LmdbRepWeightStore::new(&env).unwrap(),
                env,
            }
        }
    }

    // Tests for EpochRepWeightStore with dummy rep weights
    mod epoch_tests {
        use super::*;

        #[test]
        fn put_and_get_dummy_rep_weight() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let epoch = Epoch::Epoch0;
            let representative = PublicKey::from(1);
            let weight = Amount::from(1000);

            store
                .put_dummy(&mut txn, epoch, representative, weight)
                .unwrap();
            txn.commit();

            let txn = env.begin_read();
            let result = store.get_dummy(&txn, epoch, &representative).unwrap();
            assert_eq!(result, Some(weight));
        }

        #[test]
        fn dummy_rep_weights_separate_from_regular() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let epoch = Epoch::Epoch0;
            let representative = PublicKey::from(1);
            let regular_weight = Amount::from(500);
            let dummy_weight = Amount::from(1000);

            // Store regular rep weight
            store
                .put(&mut txn, epoch, representative, regular_weight)
                .unwrap();
            // Store dummy rep weight
            store
                .put_dummy(&mut txn, epoch, representative, dummy_weight)
                .unwrap();
            txn.commit();

            let txn = env.begin_read();
            // Regular weight should be separate
            let regular_result = store.get(&txn, epoch, &representative).unwrap();
            assert_eq!(regular_result, Some(regular_weight));
            // Dummy weight should be separate
            let dummy_result = store.get_dummy(&txn, epoch, &representative).unwrap();
            assert_eq!(dummy_result, Some(dummy_weight));
        }

        #[test]
        fn count_dummy_rep_weights() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let epoch = Epoch::Epoch1;

            store
                .put_dummy(&mut txn, epoch, PublicKey::from(1), Amount::from(100))
                .unwrap();
            store
                .put_dummy(&mut txn, epoch, PublicKey::from(2), Amount::from(200))
                .unwrap();
            store
                .put_dummy(&mut txn, epoch, PublicKey::from(3), Amount::from(300))
                .unwrap();
            txn.commit();

            let txn = env.begin_read();
            let count = store.count_dummy(&txn, epoch).unwrap();
            assert_eq!(count, 3);
        }

        #[test]
        fn iter_dummy_rep_weights() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let epoch = Epoch::Epoch2;
            let rep1 = PublicKey::from(1);
            let rep2 = PublicKey::from(2);
            let weight1 = Amount::from(100);
            let weight2 = Amount::from(200);

            store.put_dummy(&mut txn, epoch, rep1, weight1).unwrap();
            store.put_dummy(&mut txn, epoch, rep2, weight2).unwrap();
            txn.commit();

            let txn = env.begin_read();
            let iter = store.iter_dummy(&txn, epoch).unwrap();
            let mut results: Vec<_> = iter.collect();
            results.sort_by_key(|(k, _)| *k);
            assert_eq!(results.len(), 2);
            assert_eq!(results[0], (rep1, weight1));
            assert_eq!(results[1], (rep2, weight2));
        }

        #[test]
        fn delete_dummy_rep_weight() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let epoch = Epoch::Epoch0;
            let representative = PublicKey::from(1);
            let weight = Amount::from(1000);

            store
                .put_dummy(&mut txn, epoch, representative, weight)
                .unwrap();
            txn.commit();

            let mut txn = env.begin_write();
            store.del_dummy(&mut txn, epoch, &representative).unwrap();
            txn.commit();

            let txn = env.begin_read();
            let result = store.get_dummy(&txn, epoch, &representative).unwrap();
            assert_eq!(result, None);
        }

        #[test]
        fn initialized_dummy_epochs() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();

            store
                .put_dummy(
                    &mut txn,
                    Epoch::Epoch0,
                    PublicKey::from(1),
                    Amount::from(100),
                )
                .unwrap();
            store
                .put_dummy(
                    &mut txn,
                    Epoch::Epoch1,
                    PublicKey::from(2),
                    Amount::from(200),
                )
                .unwrap();
            txn.commit();

            let epochs = store.initialized_dummy_epochs();
            assert_eq!(epochs.len(), 2);
            assert!(epochs.contains(&Epoch::Epoch0));
            assert!(epochs.contains(&Epoch::Epoch1));
        }

        #[test]
        fn get_effective_weight_with_regular_weights() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let representative = PublicKey::from(1);
            let weight = Amount::from(1000);

            // Store regular weights in Epoch1
            store
                .put(&mut txn, Epoch::Epoch1, representative, weight)
                .unwrap();
            txn.commit();

            let txn = env.begin_read();
            // Should find regular weights in Epoch1
            let result = store
                .get_effective_weight(&txn, Epoch::Epoch1, &representative)
                .unwrap();
            assert_eq!(result, Some(weight));

            // Should also find them when starting from Epoch2
            let result = store
                .get_effective_weight(&txn, Epoch::Epoch2, &representative)
                .unwrap();
            assert_eq!(result, Some(weight));
        }

        #[test]
        fn get_effective_weight_fallback_from_dummy() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let representative = PublicKey::from(1);
            let weight = Amount::from(1000);

            // Store dummy weights in Epoch2
            store
                .put_dummy(&mut txn, Epoch::Epoch2, representative, Amount::from(500))
                .unwrap();
            // Store regular weights in Epoch1
            store
                .put(&mut txn, Epoch::Epoch1, representative, weight)
                .unwrap();
            txn.commit();

            let txn = env.begin_read();
            // Starting from Epoch2, should fallback to Epoch1 and find regular weights
            let result = store
                .get_effective_weight(&txn, Epoch::Epoch2, &representative)
                .unwrap();
            assert_eq!(result, Some(weight));
        }

        #[test]
        fn get_effective_weight_fallback_through_multiple_dummies() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let representative = PublicKey::from(1);
            let weight = Amount::from(1000);

            // Store dummy weights in Epoch2 and Epoch1
            store
                .put_dummy(&mut txn, Epoch::Epoch2, representative, Amount::from(500))
                .unwrap();
            store
                .put_dummy(&mut txn, Epoch::Epoch1, representative, Amount::from(600))
                .unwrap();
            // Store regular weights in Epoch0
            store
                .put(&mut txn, Epoch::Epoch0, representative, weight)
                .unwrap();
            txn.commit();

            let txn = env.begin_read();
            // Starting from Epoch2, should fallback through Epoch1 to Epoch0
            let result = store
                .get_effective_weight(&txn, Epoch::Epoch2, &representative)
                .unwrap();
            assert_eq!(result, Some(weight));
        }

        #[test]
        fn get_effective_weight_no_weights_found() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let representative = PublicKey::from(1);

            // Store dummy weights in Epoch2, but no regular weights anywhere
            store
                .put_dummy(&mut txn, Epoch::Epoch2, representative, Amount::from(500))
                .unwrap();
            txn.commit();

            let txn = env.begin_read();
            // Should fallback all the way and return None
            let result = store
                .get_effective_weight(&txn, Epoch::Epoch2, &representative)
                .unwrap();
            assert_eq!(result, None);
        }

        #[test]
        fn get_effective_weight_fallback_when_no_weights_exist() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let representative = PublicKey::from(1);
            let weight = Amount::from(1000);

            // No weights in Epoch2, but regular weights in Epoch1
            store
                .put(&mut txn, Epoch::Epoch1, representative, weight)
                .unwrap();
            txn.commit();

            let txn = env.begin_read();
            // Starting from Epoch2 (no weights), should fallback to Epoch1
            let result = store
                .get_effective_weight(&txn, Epoch::Epoch2, &representative)
                .unwrap();
            assert_eq!(result, Some(weight));
        }

        #[test]
        fn epochs_with_regular_weights() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let representative = PublicKey::from(1);

            // Add weights to Epoch0 and Epoch2, but not Epoch1
            store
                .put(&mut txn, Epoch::Epoch0, representative, Amount::from(100))
                .unwrap();
            store
                .put(&mut txn, Epoch::Epoch2, representative, Amount::from(300))
                .unwrap();
            txn.commit();

            let txn = env.begin_read();
            let epochs = store.epochs_with_regular_weights(&txn).unwrap();
            assert_eq!(epochs.len(), 2);
            assert!(epochs.contains(&Epoch::Epoch0));
            assert!(epochs.contains(&Epoch::Epoch2));
            assert!(!epochs.contains(&Epoch::Epoch1));
        }

        #[test]
        fn cleanup_old_epochs_keeps_last_two() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let representative = PublicKey::from(1);

            // Add weights to all three epochs
            store
                .put(&mut txn, Epoch::Epoch0, representative, Amount::from(100))
                .unwrap();
            store
                .put(&mut txn, Epoch::Epoch1, representative, Amount::from(200))
                .unwrap();
            store
                .put(&mut txn, Epoch::Epoch2, representative, Amount::from(300))
                .unwrap();
            txn.commit();

            let mut txn = env.begin_write();
            let removed = store.cleanup_old_epochs(&mut txn).unwrap();
            txn.commit();

            // Should have removed Epoch0 (oldest), keeping Epoch1 and Epoch2
            assert_eq!(removed.len(), 1);
            assert_eq!(removed[0], Epoch::Epoch0);

            // Verify Epoch0 is gone, but Epoch1 and Epoch2 remain
            let txn = env.begin_read();
            let epochs = store.epochs_with_regular_weights(&txn).unwrap();
            assert_eq!(epochs.len(), 2);
            assert!(epochs.contains(&Epoch::Epoch1));
            assert!(epochs.contains(&Epoch::Epoch2));
            assert!(!epochs.contains(&Epoch::Epoch0));

            // Verify weights are gone from Epoch0
            assert_eq!(
                store.get(&txn, Epoch::Epoch0, &representative).unwrap(),
                None
            );
            // But still exist in Epoch1 and Epoch2
            assert_eq!(
                store.get(&txn, Epoch::Epoch1, &representative).unwrap(),
                Some(Amount::from(200))
            );
            assert_eq!(
                store.get(&txn, Epoch::Epoch2, &representative).unwrap(),
                Some(Amount::from(300))
            );
        }

        #[test]
        fn cleanup_old_epochs_no_change_when_two_or_fewer() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let representative = PublicKey::from(1);

            // Add weights to only two epochs
            store
                .put(&mut txn, Epoch::Epoch1, representative, Amount::from(200))
                .unwrap();
            store
                .put(&mut txn, Epoch::Epoch2, representative, Amount::from(300))
                .unwrap();
            txn.commit();

            let mut txn = env.begin_write();
            let removed = store.cleanup_old_epochs(&mut txn).unwrap();
            txn.commit();

            // Should not remove anything
            assert_eq!(removed.len(), 0);

            // Verify both epochs still exist
            let txn = env.begin_read();
            let epochs = store.epochs_with_regular_weights(&txn).unwrap();
            assert_eq!(epochs.len(), 2);
        }

        #[test]
        fn cleanup_old_epochs_preserves_dummy_weights() {
            let env = Arc::new(LmdbEnvironment::new_null());
            let store = EpochRepWeightStore::new(env.clone());
            let mut txn = env.begin_write();
            let representative = PublicKey::from(1);

            // Add regular weights to Epoch0 and Epoch1
            store
                .put(&mut txn, Epoch::Epoch0, representative, Amount::from(100))
                .unwrap();
            store
                .put(&mut txn, Epoch::Epoch1, representative, Amount::from(200))
                .unwrap();
            // Add dummy weights to Epoch0
            store
                .put_dummy(&mut txn, Epoch::Epoch0, representative, Amount::from(500))
                .unwrap();
            txn.commit();

            let mut txn = env.begin_write();
            // This should not remove anything since we only have 2 epochs with regular weights
            let removed = store.cleanup_old_epochs(&mut txn).unwrap();
            txn.commit();

            assert_eq!(removed.len(), 0);

            // Verify dummy weights are still there
            let txn = env.begin_read();
            assert_eq!(
                store
                    .get_dummy(&txn, Epoch::Epoch0, &representative)
                    .unwrap(),
                Some(Amount::from(500))
            );
        }
    }
}
