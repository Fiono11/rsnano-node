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
    min_weight: Amount,
}

impl RepWeightsUpdater {
    pub fn new(store: Arc<LmdbRepWeightStore>, min_weight: Amount, cache: &RepWeightCache) -> Self {
        RepWeightsUpdater {
            weight_cache: cache.inner(),
            store,
            min_weight,
        }
    }

    /// Only use this method when loading rep weights from the database table
    pub fn copy_from(&self, other: &HashMap<PublicKey, Amount>) {
        let mut guard_this = self.weight_cache.write().unwrap();
        for (account, amount) in other {
            let prev_amount = self.get(&guard_this, account);
            self.put_cache(&mut guard_this, *account, prev_amount.wrapping_add(*amount));
        }
    }

    /// Only use this method when loading rep weights from the database table!
    pub fn put(&self, representative: PublicKey, weight: Amount) {
        let mut guard = self.weight_cache.write().unwrap();
        self.put_cache(&mut guard, representative, weight);
    }

    pub fn add(&self, txn: &mut WriteTransaction, representative: PublicKey, amount: Amount) {
        let previous_weight = self.store.get(txn, &representative).unwrap_or_default();
        let new_weight = previous_weight.wrapping_add(amount);
        self.put_store(txn, representative, previous_weight, new_weight);
        let mut guard = self.weight_cache.write().unwrap();
        self.put_cache(&mut guard, representative, new_weight);
    }

    pub fn sub(&self, txn: &mut WriteTransaction, representative: PublicKey, amount: Amount) {
        self.add(txn, representative, Amount::ZERO.wrapping_sub(amount))
    }

    pub fn sub_and_add(
        &self,
        txn: &mut WriteTransaction,
        sub_rep: PublicKey,
        sub_amount: Amount,
        add_rep: PublicKey,
        add_amount: Amount,
    ) {
        if sub_rep != add_rep {
            let previous_weight_1 = self.store.get(txn, &sub_rep).unwrap_or_default();
            let previous_weight_2 = self.store.get(txn, &add_rep).unwrap_or_default();
            let new_weight_1 = previous_weight_1.wrapping_sub(sub_amount);
            let new_weight_2 = previous_weight_2.wrapping_add(add_amount);
            self.put_store(txn, sub_rep, previous_weight_1, new_weight_1);
            self.put_store(txn, add_rep, previous_weight_2, new_weight_2);
            let mut guard = self.weight_cache.write().unwrap();
            self.put_cache(&mut guard, sub_rep, new_weight_1);
            self.put_cache(&mut guard, add_rep, new_weight_2);
        } else {
            self.add(txn, sub_rep, add_amount.wrapping_sub(sub_amount));
        }
    }

    pub fn add_dual(
        &self,
        txn: &mut WriteTransaction,
        rep_1: PublicKey,
        amount_1: Amount,
        rep_2: PublicKey,
        amount_2: Amount,
    ) {
        if rep_1 != rep_2 {
            let previous_weight_1 = self.store.get(txn, &rep_1).unwrap_or_default();
            let previous_weight_2 = self.store.get(txn, &rep_2).unwrap_or_default();
            let new_weight_1 = previous_weight_1.wrapping_add(amount_1);
            let new_weight_2 = previous_weight_2.wrapping_add(amount_2);
            self.put_store(txn, rep_1, previous_weight_1, new_weight_1);
            self.put_store(txn, rep_2, previous_weight_2, new_weight_2);
            let mut guard = self.weight_cache.write().unwrap();
            self.put_cache(&mut guard, rep_1, new_weight_1);
            self.put_cache(&mut guard, rep_2, new_weight_2);
        } else {
            self.add(txn, rep_1, amount_1.wrapping_add(amount_2));
        }
    }

    fn put_cache(
        &self,
        weights: &mut HashMap<PublicKey, Amount>,
        representative: PublicKey,
        new_weight: Amount,
    ) {
        if new_weight < self.min_weight || new_weight.is_zero() {
            weights.remove(&representative);
        } else {
            weights.insert(representative, new_weight);
        }
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

        fixture.updater.put(account, Amount::from(1));
        assert_eq!(fixture.weights.weight(&account), Amount::from(1));

        fixture.updater.put(account, Amount::from(2));
        assert_eq!(fixture.weights.weight(&account), Amount::from(2));
    }

    #[test]
    fn delete_rep_weight_of_zero() {
        let representative = PublicKey::from(1);
        let weight = Amount::from(100);

        let fixture = create_fixture(0, vec![(representative, weight)]);
        let delete_tracker = fixture.store.track_deletions();
        fixture.updater.put(representative, weight);
        let mut txn = fixture.env.begin_write();

        // set weight to 0
        fixture.updater.sub(&mut txn, representative, weight);
        txn.commit();

        assert_eq!(fixture.weights.len(), 0);
        assert_eq!(delete_tracker.output(), vec![representative]);
    }

    #[test]
    fn delete_rep_weight_of_zero_dual() {
        let rep1 = PublicKey::from(1);
        let rep2 = PublicKey::from(2);
        let weight = Amount::from(100);

        let fixture = create_fixture(0, vec![(rep1, weight), (rep2, weight)]);
        let delete_tracker = fixture.store.track_deletions();
        fixture.updater.put(rep1, weight);
        fixture.updater.put(rep2, weight);
        let mut txn = fixture.env.begin_write();

        // set weight to 0
        fixture.updater.add_dual(
            &mut txn,
            rep1,
            Amount::ZERO.wrapping_sub(weight),
            rep2,
            Amount::ZERO.wrapping_sub(weight),
        );
        txn.commit();

        assert_eq!(fixture.weights.len(), 0);
        assert_eq!(delete_tracker.output(), vec![rep1, rep2]);
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
        let rep_weights = RepWeightCache::new();
        let updater = RepWeightsUpdater::new(store.clone(), min_weight, &rep_weights);

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
