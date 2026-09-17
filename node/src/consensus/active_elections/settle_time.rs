use std::time::Duration;

/// Running estimate of how long an election takes from its start until it
/// settles, from the elections that did settle: an exponential moving average
/// of the duration and of its absolute deviation. The closer solicits an
/// election's votes only once it has been open longer than the threshold and
/// is still unsettled, which is the sign that a vote was lost, so requests
/// track the actual pace of finalization instead of a fixed timer.
#[derive(Clone, Debug)]
pub(crate) struct SettleTime {
    mean_ns: f64,
    deviation_ns: f64,
    samples: u64,
}

impl SettleTime {
    /// Weight of each new sample: recent load dominates, one election never does.
    const ALPHA: f64 = 1.0 / 32.0;
    /// Deviations above the mean that an election may take before it is
    /// suspected of a lost vote.
    const DEVIATIONS: f64 = 3.0;
    const MIN: Duration = Duration::from_millis(200);
    const MAX: Duration = Duration::from_secs(10);

    pub fn new() -> Self {
        Self {
            mean_ns: Duration::from_secs(1).as_nanos() as f64,
            deviation_ns: Duration::from_millis(250).as_nanos() as f64,
            samples: 0,
        }
    }

    pub fn record(&mut self, duration: Duration) {
        let sample = duration.as_nanos() as f64;
        let error = sample - self.mean_ns;
        self.mean_ns += Self::ALPHA * error;
        self.deviation_ns += Self::ALPHA * (error.abs() - self.deviation_ns);
        self.samples += 1;
    }

    pub fn mean(&self) -> Duration {
        Duration::from_nanos(self.mean_ns as u64)
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// How long an election may stay unsettled before its votes are requested.
    pub fn threshold(&self) -> Duration {
        Duration::from_nanos((self.mean_ns + Self::DEVIATIONS * self.deviation_ns) as u64)
            .clamp(Self::MIN, Self::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_follows_the_settlement_pace_with_a_margin() {
        let mut estimate = SettleTime::new();
        assert_eq!(estimate.threshold(), Duration::from_millis(1750));
        for _ in 0..200 {
            estimate.record(Duration::from_millis(300));
        }
        assert!((290..=310).contains(&(estimate.mean().as_millis() as i64)));
        assert!(estimate.threshold() < Duration::from_millis(400));
        assert!(estimate.threshold() >= Duration::from_millis(300));
        // A spread of settlement times widens the margin.
        for i in 0..200 {
            estimate.record(Duration::from_millis(if i % 2 == 0 { 100 } else { 900 }));
        }
        assert!(estimate.threshold() > Duration::from_millis(900));
        assert_eq!(estimate.samples(), 400);
    }

    #[test]
    fn threshold_is_clamped() {
        let mut estimate = SettleTime::new();
        for _ in 0..500 {
            estimate.record(Duration::from_millis(1));
        }
        assert_eq!(estimate.threshold(), SettleTime::MIN);
        for _ in 0..500 {
            estimate.record(Duration::from_secs(60));
        }
        assert_eq!(estimate.threshold(), SettleTime::MAX);
    }
}
