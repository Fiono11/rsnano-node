use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use rsnano_ledger::Ledger;
use rsnano_nullable_clock::SystemTimeFactory;
use rsnano_nullable_lmdb::WriteTransaction;
#[cfg(test)]
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_types::{Amount, NetworkType};

pub struct TrendResult {
    pub trended: Amount,
    pub sample_count: usize,
}

pub struct OnlineWeightSampler {
    system_time_factory: SystemTimeFactory,
    ledger: Arc<Ledger>,

    /// The maximum time to keep online weight samples
    cutoff: Duration,
    #[cfg(test)]
    sample_listener: OutputListenerMt<Amount>,
}

impl OnlineWeightSampler {
    pub fn new(ledger: Arc<Ledger>, network: NetworkType) -> Self {
        Self::new_impl(SystemTimeFactory::default(), ledger, network)
    }

    pub fn new_null() -> Self {
        let trended = Amount::nano(90_000_000);
        Self::new_null_with_trended_weight(trended)
    }

    pub fn new_null_with_trended_weight(trended: Amount) -> Self {
        let ledger = Ledger::new_null();
        let time_factory = SystemTimeFactory::new_null();
        let sampler = Self::new_impl(time_factory, ledger.into(), NetworkType::NanoLiveNetwork);
        sampler.add_sample(trended);
        sampler
    }

    fn new_impl(
        system_time_factory: SystemTimeFactory,
        ledger: Arc<Ledger>,
        network: NetworkType,
    ) -> Self {
        Self {
            system_time_factory,
            ledger,
            cutoff: Self::cutoff_for(network),
            #[cfg(test)]
            sample_listener: OutputListenerMt::default(),
        }
    }

    fn cutoff_for(network: NetworkType) -> Duration {
        match network {
            NetworkType::NanoLiveNetwork | NetworkType::NanoTestNetwork => {
                // Two weeks
                Duration::from_hours(24 * 7 * 2)
            }
            _ => {
                // One day
                Duration::from_hours(24)
            }
        }
    }

    #[cfg(test)]
    pub fn track_samples(&self) -> Arc<OutputTrackerMt<Amount>> {
        self.sample_listener.track()
    }

    pub fn calculate_trend(&self) -> TrendResult {
        let samples = self.load_samples();
        let sample_count = samples.len();
        let trended = self.medium_weight(samples);
        TrendResult {
            trended,
            sample_count,
        }
    }

    fn load_samples(&self) -> Vec<Amount> {
        let txn = self.ledger.store.begin_read();
        self.ledger
            .store
            .online_weight
            .iter(&txn)
            .map(|(_, amount)| amount)
            .collect()
    }

    fn medium_weight(&self, mut items: Vec<Amount>) -> Amount {
        if items.is_empty() {
            Amount::ZERO
        } else {
            let median_idx = items.len() / 2;
            items.sort();
            items[median_idx]
        }
    }

    /// Called periodically to sample online weight
    pub fn add_sample(&self, current_online_weight: Amount) {
        #[cfg(test)]
        {
            self.sample_listener.emit(current_online_weight);
        }
        let now = self.system_time_factory.now();
        let mut txn = self.ledger.store.begin_write();
        self.sanitize_samples(&mut txn, now);
        self.insert_new_sample(&mut txn, current_online_weight, now);
        txn.commit();
    }

    pub fn sanitize(&self) {
        let now = self.system_time_factory.now();
        let mut txn = self.ledger.store.begin_write();
        self.sanitize_samples(&mut txn, now);
        txn.commit();
    }

    fn sanitize_samples(&self, tx: &mut WriteTransaction, now: SystemTime) {
        let to_delete = self.samples_to_delete(tx, now);

        for timestamp in to_delete {
            self.ledger.store.online_weight.del(tx, timestamp);
        }
    }

    fn samples_to_delete(&self, tx: &WriteTransaction, now: SystemTime) -> Vec<u64> {
        let mut to_delete = Vec::new();
        to_delete.extend(self.old_samples(tx, now));
        to_delete.extend(self.future_samples(tx, now));
        to_delete
    }

    fn old_samples<'tx>(
        &self,
        tx: &'tx WriteTransaction,
        now: SystemTime,
    ) -> impl Iterator<Item = u64> + use<'tx> {
        let timestamp_cutoff = system_time_as_seconds(now - self.cutoff);

        self.ledger
            .store
            .online_weight
            .iter(tx)
            .map(|(ts, _)| ts)
            .take_while(move |ts| *ts < timestamp_cutoff)
    }

    fn future_samples<'tx>(
        &self,
        tx: &'tx WriteTransaction,
        now: SystemTime,
    ) -> impl Iterator<Item = u64> + use<'tx> {
        let timestamp_now = system_time_as_seconds(now);

        self.ledger
            .store
            .online_weight
            .iter_rev(tx)
            .map(|(ts, _)| ts)
            .take_while(move |ts| *ts > timestamp_now)
    }

    fn insert_new_sample(
        &self,
        txn: &mut WriteTransaction,
        current_online_weight: Amount,
        now: SystemTime,
    ) {
        self.ledger.store.online_weight.put(
            txn,
            system_time_as_seconds(now),
            &current_online_weight,
        );
    }
}

fn system_time_as_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_sample_can_be_tracked() {
        let time_factory = SystemTimeFactory::new_null();
        let ledger = Arc::new(Ledger::new_null());
        let sampler =
            OnlineWeightSampler::new_impl(time_factory, ledger, NetworkType::NanoLiveNetwork);
        let sample_tracker = sampler.track_samples();
        let online_weight = Amount::nano(12345);

        sampler.add_sample(online_weight);

        assert_eq!(sample_tracker.output(), vec![online_weight]);
    }

    /*
     * Nullability
     */

    #[test]
    fn can_be_nulled() {
        let sampler = OnlineWeightSampler::new_null();
        assert_eq!(sampler.calculate_trend().trended, Amount::nano(90_000_000));
    }

    #[test]
    fn nulled_sampler_can_be_configured() {
        let trended = Amount::nano(98_700_000);
        let sampler = OnlineWeightSampler::new_null_with_trended_weight(trended);
        assert_eq!(sampler.calculate_trend().trended, trended);
    }
}
