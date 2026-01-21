use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use rsnano_nullable_lmdb::WriteTransaction;
use rsnano_store_lmdb::LmdbRepWeightStore;
use rsnano_types::{Amount, PublicKey};

use crate::{RepWeightCache, RepWeights};

/// Updates the representative weights in the ledger and in the in-memory cache
pub struct RepWeightsUpdater {
    weight_cache: Arc<RwLock<RepWeights>>,
    store: Arc<LmdbRepWeightStore>,
}

impl RepWeightsUpdater {
    pub fn new(store: Arc<LmdbRepWeightStore>, cache: &RepWeightCache) -> Self {
        RepWeightsUpdater {
            weight_cache: cache.inner(),
            store,
        }
    }

    /// Only use this method when loading rep weights from the database table
    pub fn copy_from(&self, other: &HashMap<PublicKey, Amount>) {
        let mut cache = self.weight_cache.write().unwrap();
        for (account, amount) in other {
            let prev_amount = self.get(&cache, account);
            cache.put(*account, prev_amount.wrapping_add(*amount))
        }
    }

    /// Only use this method when loading rep weights from the database table!
    pub fn put_cache(&self, representative: PublicKey, weight: Amount) {
        self.weight_cache
            .write()
            .unwrap()
            .put(representative, weight);
    }

    pub fn add(&self, txn: &mut WriteTransaction, representative: PublicKey, amount: Amount) {
        let (old_weight, new_weight) = self.prepare_add(txn, &representative, amount);
        self.put(txn, representative, old_weight, new_weight);
    }

    pub fn sub(&self, txn: &mut WriteTransaction, representative: PublicKey, amount: Amount) {
        let (old_weight, new_weight) = self.prepare_sub(txn, &representative, amount);
        self.put(txn, representative, old_weight, new_weight)
    }

    /// Subtract sub_amount from sub_rep and add add_amount to add_rep
    pub fn sub_and_add(
        &self,
        txn: &mut WriteTransaction,
        sub_rep: PublicKey,
        sub_amount: Amount,
        add_rep: PublicKey,
        add_amount: Amount,
    ) {
        if sub_rep != add_rep {
            let (old_sub_weight, new_sub_weight) = self.prepare_sub(txn, &sub_rep, sub_amount);
            let (old_add_weight, new_add_weight) = self.prepare_add(txn, &add_rep, add_amount);
            self.put_store(txn, sub_rep, old_sub_weight, new_sub_weight);
            self.put_store(txn, add_rep, old_add_weight, new_add_weight);
            let mut cache = self.weight_cache.write().unwrap();
            cache.put(sub_rep, new_sub_weight);
            cache.put(add_rep, new_add_weight);
        } else {
            if add_amount >= sub_amount {
                self.add(txn, add_rep, add_amount - sub_amount);
            } else {
                self.sub(txn, add_rep, sub_amount - add_amount);
            }
        }
    }

    fn prepare_add(
        &self,
        txn: &mut WriteTransaction,
        representative: &PublicKey,
        amount: Amount,
    ) -> (Amount, Amount) {
        let old_weight = self.store.get(txn, representative).unwrap_or_default();
        let Some(new_weight) = old_weight.checked_add(amount) else {
            panic!(
                "Increasing rep weight caused an overflow (rep={}, amount={}, old weight={})",
                representative.as_account().encode_hex(),
                amount.number(),
                old_weight.number()
            );
        };

        (old_weight, new_weight)
    }

    fn prepare_sub(
        &self,
        txn: &mut WriteTransaction,
        representative: &PublicKey,
        amount: Amount,
    ) -> (Amount, Amount) {
        let old_weight = self.store.get(txn, representative).unwrap_or_default();
        let Some(new_weight) = old_weight.checked_sub(amount) else {
            panic!(
                "Decreasing rep weight caused an underflow (rep={}, amount={}, old weight={})",
                representative.as_account().encode_hex(),
                amount.number(),
                old_weight.number()
            );
        };

        (old_weight, new_weight)
    }

    fn put(
        &self,
        txn: &mut WriteTransaction,
        rep: PublicKey,
        old_weight: Amount,
        new_weight: Amount,
    ) {
        self.put_store(txn, rep, old_weight, new_weight);
        self.weight_cache.write().unwrap().put(rep, new_weight);
    }

    fn put_store(
        &self,
        txn: &mut WriteTransaction,
        representative: PublicKey,
        previous_weight: Amount,
        new_weight: Amount,
    ) {
        if new_weight.is_zero() {
            if !previous_weight.is_zero() {
                self.store.del(txn, &representative);
            }
        } else {
            self.store.put(txn, representative, new_weight);
        }
    }

    fn get(&self, weights: &HashMap<PublicKey, Amount>, account: &PublicKey) -> Amount {
        weights.get(account).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_lmdb::LmdbEnvironment;
    use rsnano_store_lmdb::ConfiguredRepWeightDatabaseBuilder;

    #[test]
    fn representation_changes() {
        let fixture = create_fixture(0, vec![]);
        let account = PublicKey::from(1);
        assert_eq!(fixture.weights.weight(&account), Amount::ZERO);

        fixture.updater.put_cache(account, Amount::from(1));
        assert_eq!(fixture.weights.weight(&account), Amount::from(1));

        fixture.updater.put_cache(account, Amount::from(2));
        assert_eq!(fixture.weights.weight(&account), Amount::from(2));
    }

    #[test]
    fn delete_rep_weight_of_zero() {
        let representative = PublicKey::from(1);
        let weight = Amount::from(100);

        let fixture = create_fixture(0, vec![(representative, weight)]);
        let delete_tracker = fixture.store.track_deletions();
        fixture.updater.put_cache(representative, weight);
        let mut txn = fixture.env.begin_write();

        // set weight to 0
        fixture.updater.sub(&mut txn, representative, weight);
        txn.commit();

        assert_eq!(fixture.weights.len(), 0);
        assert_eq!(delete_tracker.output(), vec![representative]);
    }

    #[test]
    fn add_below_min_weight() {
        let fixture = create_fixture(10, vec![]);
        let put_tracker = fixture.store.track_puts();
        let mut txn = fixture.env.begin_write();
        let representative = PublicKey::from(1);
        let rep_weight = Amount::from(9);

        fixture.updater.add(&mut txn, representative, rep_weight);
        txn.commit();

        assert_eq!(fixture.weights.len(), 0);
        assert_eq!(put_tracker.output(), vec![(representative, rep_weight)]);
    }

    #[test]
    #[should_panic(
        expected = "Increasing rep weight caused an overflow (rep=0000000000000000000000000000000000000000000000000000000000000001, amount=2, old weight=340282366920938463463374607431768211454)"
    )]
    fn add_overflow() {
        let rep = PublicKey::from(1);
        let weight = Amount::MAX - Amount::raw(1);
        let fixture = create_fixture(10, vec![(rep, weight)]);
        let updater = &fixture.updater;
        let mut txn = fixture.env.begin_write();

        updater.add(&mut txn, rep, Amount::raw(2));
    }

    #[test]
    fn fall_below_min_weight() {
        let representative = PublicKey::from(1);
        let weight = Amount::from(11);

        let fixture = create_fixture(10, vec![(representative, weight)]);
        let put_tracker = fixture.store.track_puts();
        let mut txn = fixture.env.begin_write();

        fixture
            .updater
            .sub(&mut txn, representative, Amount::from(2));

        txn.commit();

        assert_eq!(fixture.weights.len(), 0);
        assert_eq!(put_tracker.output(), vec![(representative, 9.into())]);
    }

    #[test]
    #[should_panic(
        expected = "Decreasing rep weight caused an underflow (rep=0000000000000000000000000000000000000000000000000000000000000001, amount=2, old weight=1)"
    )]
    fn sub_underflow() {
        let rep = PublicKey::from(1);
        let weight = Amount::raw(1);
        let fixture = create_fixture(10, vec![(rep, weight)]);
        let updater = &fixture.updater;
        let mut txn = fixture.env.begin_write();

        updater.sub(&mut txn, rep, Amount::raw(2));
    }

    #[test]
    fn sub_and_add_same_rep() {
        let representative = PublicKey::from(1);

        let fixture = create_fixture(0, vec![(representative, Amount::raw(10))]);

        let mut txn = fixture.env.begin_write();
        fixture.updater.sub_and_add(
            &mut txn,
            representative,
            Amount::raw(1),
            representative,
            Amount::raw(3),
        );

        assert_eq!(fixture.weights.weight(&representative), Amount::raw(12));
    }

    #[test]
    fn sub_and_add_same_rep_underflow() {
        let representative = PublicKey::from(1);

        let fixture = create_fixture(0, vec![(representative, Amount::raw(10))]);

        let mut txn = fixture.env.begin_write();
        fixture.updater.sub_and_add(
            &mut txn,
            representative,
            Amount::raw(15),
            representative,
            Amount::raw(10),
        );

        assert_eq!(fixture.weights.weight(&representative), Amount::raw(5));
    }

    #[test]
    fn sub_and_add_two_reps() {
        let rep1 = PublicKey::from(1);
        let rep2 = PublicKey::from(2);

        let fixture = create_fixture(0, vec![(rep1, Amount::raw(10)), (rep2, Amount::raw(50))]);

        let mut txn = fixture.env.begin_write();
        fixture
            .updater
            .sub_and_add(&mut txn, rep1, Amount::raw(8), rep2, Amount::raw(100));

        assert_eq!(fixture.weights.weight(&rep1), Amount::raw(2));
        assert_eq!(fixture.weights.weight(&rep2), Amount::raw(150));
    }

    fn create_fixture(min_weight_raw: u128, weights: Vec<(PublicKey, Amount)>) -> Fixture {
        let env = LmdbEnvironment::null_builder()
            .configured_database(ConfiguredRepWeightDatabaseBuilder::create(weights))
            .build();

        let store = Arc::new(LmdbRepWeightStore::new(&env).unwrap());
        let min_weight = Amount::raw(min_weight_raw);
        let rep_weights = RepWeightCache::new(min_weight);
        let updater = RepWeightsUpdater::new(store.clone(), &rep_weights);

        Fixture {
            updater,
            env,
            weights: rep_weights,
            store,
        }
    }

    struct Fixture {
        updater: RepWeightsUpdater,
        env: LmdbEnvironment,
        weights: RepWeightCache,
        store: Arc<LmdbRepWeightStore>,
    }
}
