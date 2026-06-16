use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tracing::info;

use rsnano_nullable_clock::SteadyClock;
use rsnano_utils::{CancellationToken, ticker::Tickable};

use super::{OnlineWeightSampler, RepresentativeTracker};

pub struct OnlineWeightCalculation {
    sampler: OnlineWeightSampler,
    rep_tracker: Arc<RepresentativeTracker>,
    clock: Arc<SteadyClock>,
    first_run: bool,
    last_sample: Instant,
}

impl OnlineWeightCalculation {
    pub fn new(
        sampler: OnlineWeightSampler,
        rep_tracker: Arc<RepresentativeTracker>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            sampler,
            rep_tracker,
            clock,
            first_run: true,
            last_sample: Instant::now(),
        }
    }

    fn calculate_trended_weight(&mut self) {
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
        if self.first_run {
            // Don't sample online weight on first run, because it is always 0
            self.sampler.sanitize();
            self.last_sample = Instant::now();
            self.calculate_trended_weight();
            self.first_run = false;
        } else {
            {
                self.rep_tracker.trim(self.clock.now());
                self.rep_tracker.calculate_online_weight();
            }
            if self.last_sample.elapsed() > Duration::from_secs(60) {
                let online_weight = self.rep_tracker.online_weight();
                self.sampler.add_sample(online_weight);
                self.calculate_trended_weight();
                self.last_sample = Instant::now();
            }
        }
    }
}
