use std::{sync::Arc, time::Duration};

use tracing::{info, warn};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_utils::{CancellationToken, ticker::Tickable};

use super::{OnlineWeightSampler, RepresentativeTracker};

pub struct OnlineWeightCalculation {
    clock: SteadyClock,
    sampler: OnlineWeightSampler,
    rep_tracker: Arc<RepresentativeTracker>,
    first_run: bool,
    last_sample: Timestamp,
}

impl OnlineWeightCalculation {
    pub fn new(sampler: OnlineWeightSampler, rep_tracker: Arc<RepresentativeTracker>) -> Self {
        Self::new_impl(SteadyClock::default(), sampler, rep_tracker)
    }

    fn new_impl(
        clock: SteadyClock,
        sampler: OnlineWeightSampler,
        rep_tracker: Arc<RepresentativeTracker>,
    ) -> Self {
        let now = clock.now();
        Self {
            clock,
            sampler,
            rep_tracker,
            first_run: true,
            last_sample: now,
        }
    }

    fn update_trended_weight(&mut self) {
        let result = self.sampler.calculate_trend();
        info!(
            "Trended weight updated: {}, samples: {}",
            result.trended.format_balance(0),
            result.sample_count
        );
        self.rep_tracker.set_trended(result.trended);
    }
}

impl Tickable for OnlineWeightCalculation {
    fn tick(&mut self, _: &CancellationToken) {
        let now = self.clock.now();
        if self.first_run {
            // Don't sample online weight on first run, because it is always 0
            self.sampler.sanitize();
            self.last_sample = now;
            self.update_trended_weight();
            self.first_run = false;
        } else {
            self.rep_tracker.trim();

            if self.last_sample.elapsed(now) > Duration::from_secs(60) {
                let quorum_snapshot = self.rep_tracker.quorum_snapshot();
                if quorum_snapshot.online_weight >= quorum_snapshot.online_weight_minimum {
                    self.sampler.add_sample(quorum_snapshot.online_weight);
                    self.update_trended_weight();
                } else {
                    warn!(
                        "Current online weight {} is below minimum threshold {}. \
                        This often occurs when the node cannot reach enough peers; \
                        check network connectivity and peer count.",
                        quorum_snapshot.online_weight.format_balance(0),
                        quorum_snapshot.online_weight_minimum.format_balance(0)
                    )
                }
                self.last_sample = now;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::Amount;
    use tracing_test::traced_test;

    #[test]
    #[traced_test]
    fn only_updates_trended_weight_on_first_run() {
        let clock = SteadyClock::new_null();
        let trended = Amount::nano(90_000_000);
        let sampler = OnlineWeightSampler::new_null_with_trended_weight(trended);
        let sample_tracker = sampler.track_samples();
        let rep_tracker = Arc::new(RepresentativeTracker::new_null());
        let mut calc = OnlineWeightCalculation::new_impl(clock, sampler, rep_tracker.clone());

        let ct = CancellationToken::new_null();
        calc.tick(&ct);

        assert_eq!(rep_tracker.quorum_snapshot().trended_or_min_weight, trended);
        assert_eq!(sample_tracker.output(), vec![]);
        assert!(logs_contain(
            "Trended weight updated: 90,000,000, samples: 1"
        ));
    }

    #[test]
    fn skips_sampling_if_not_enough_time_has_passed() {
        let clock = SteadyClock::new_null();
        let trended = Amount::nano(90_000_000);
        let sampler = OnlineWeightSampler::new_null_with_trended_weight(trended);
        let sample_tracker = sampler.track_samples();
        let rep_tracker = Arc::new(RepresentativeTracker::new_null());
        let mut calc = OnlineWeightCalculation::new_impl(clock, sampler, rep_tracker.clone());

        let ct = CancellationToken::new_null();
        calc.tick(&ct);
        calc.clock.advance(Duration::from_secs(10));
        calc.tick(&ct);

        assert_eq!(sample_tracker.output(), vec![]);
    }

    #[test]
    fn samples_online_weight_after_60s() {
        let clock = SteadyClock::new_null();
        let trended_weight = Amount::nano(90_000_000);
        let online_weight = Amount::nano(80_000_000);
        let sampler = OnlineWeightSampler::new_null_with_trended_weight(trended_weight);
        let sample_tracker = sampler.track_samples();
        let rep_tracker = Arc::new(RepresentativeTracker::new_null_with_peered_weight(
            online_weight,
        ));
        let mut calc = OnlineWeightCalculation::new_impl(clock, sampler, rep_tracker);

        let ct = CancellationToken::new_null();
        calc.tick(&ct);
        calc.clock.advance(Duration::from_secs(61));
        calc.tick(&ct);

        assert_eq!(sample_tracker.output(), vec![online_weight]);
    }

    #[test]
    #[traced_test]
    fn skips_sampling_if_online_weight_below_minimum() {
        let clock = SteadyClock::new_null();
        let trended_weight = Amount::nano(90_000_000);
        let online_weight = Amount::nano(1_000);
        let sampler = OnlineWeightSampler::new_null_with_trended_weight(trended_weight);
        let sample_tracker = sampler.track_samples();
        let rep_tracker = Arc::new(RepresentativeTracker::new_null_with_peered_weight(
            online_weight,
        ));
        let mut calc = OnlineWeightCalculation::new_impl(clock, sampler, rep_tracker);

        let ct = CancellationToken::new_null();
        calc.tick(&ct);
        calc.clock.advance(Duration::from_secs(61));
        calc.tick(&ct);

        assert_eq!(sample_tracker.output(), vec![]);
        assert!(logs_contain(
            "Current online weight 1,000 is below minimum threshold 60,000,000. \
            This often occurs when the node cannot reach enough peers; \
            check network connectivity and peer count."
        ));
    }
}
