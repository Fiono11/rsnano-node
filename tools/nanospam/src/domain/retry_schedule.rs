use std::time::Duration;

/// When a setup wait runs its recovery step: first after `first`, then every
/// `every` after the previous one, however long one poll of the nodes takes.
/// (A time window such as `elapsed % 5 s < 200 ms` is only hit when a poll is
/// shorter than the window: with nodes 300 ms away it hardly ever is.)
pub(crate) struct RetrySchedule {
    next: Duration,
    every: Duration,
}

impl RetrySchedule {
    pub(crate) fn new(first: Duration, every: Duration) -> Self {
        Self { next: first, every }
    }

    /// Whether the recovery is due at `elapsed`; when it is, the next one is
    /// scheduled `every` later
    pub(crate) fn due(&mut self, elapsed: Duration) -> bool {
        if elapsed < self.next {
            return false;
        }
        self.next = elapsed + self.every;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_before_the_first_delay() {
        let mut schedule = RetrySchedule::new(secs(10), secs(5));
        assert!(!schedule.due(secs(0)));
        assert!(!schedule.due(Duration::from_millis(9_999)));
        assert!(schedule.due(secs(10)));
    }

    #[test]
    fn every_interval_after_the_previous_retry_whatever_the_poll_length() {
        let mut schedule = RetrySchedule::new(secs(10), secs(5));
        // polls 1.3 s apart, as with six nodes up to 330 ms away
        let due: Vec<u64> = (0..40)
            .map(|i| Duration::from_millis(i * 1300))
            .filter(|&elapsed| schedule.due(elapsed))
            .map(|elapsed| elapsed.as_millis() as u64)
            .collect();
        assert_eq!(
            due,
            [
                10_400, 15_600, 20_800, 26_000, 31_200, 36_400, 41_600, 46_800
            ]
        );
    }

    /* Test helpers */

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }
}
