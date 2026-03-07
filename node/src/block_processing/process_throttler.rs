use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
    time::Duration,
};

use tracing::warn;

use rsnano_nullable_clock::{SteadyClock, Timestamp};

use super::BlockProcessorQueue;
use rsnano_utils::stats::{StatsCollection, StatsSource};

const THROTTLE_WAIT: Duration = Duration::from_millis(100);

/// Waits until a condition is met (the `should_throttle` callback returns false)
pub(crate) struct ProcessThrottler {
    queue: Arc<BlockProcessorQueue>,
    should_throttle: Box<dyn Fn() -> bool + Send + Sync>,
    call_count: AtomicUsize,
    cooldown_count: AtomicUsize,
    last_log: Mutex<Option<Timestamp>>,
    clock: Arc<SteadyClock>,
}

impl ProcessThrottler {
    pub fn new(
        queue: Arc<BlockProcessorQueue>,
        clock: Arc<SteadyClock>,
        should_throttle: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            queue,
            should_throttle: Box::new(should_throttle),
            call_count: AtomicUsize::new(0),
            cooldown_count: AtomicUsize::new(0),
            last_log: Mutex::new(None),
            clock,
        }
    }

    #[cfg(test)]
    pub fn new_null() -> Self {
        let queue = Arc::new(BlockProcessorQueue::new_null());
        let clock = Arc::new(SteadyClock::new_null());
        Self::new(queue, clock, || false)
    }

    pub fn wait_for_backlog(&self) {
        self.call_count.fetch_add(1, Relaxed);
        if !(self.should_throttle)() {
            return;
        }

        let now = self.clock.now();
        if self.should_log(now) {
            warn!("Throttling block processing!");
        }

        self.cooldown_count.fetch_add(1, Relaxed);
        self.queue.wait(THROTTLE_WAIT);
    }

    fn should_log(&self, now: Timestamp) -> bool {
        let mut last_log = self.last_log.lock().unwrap();
        let should_log = match *last_log {
            Some(i) => i.elapsed(now) >= Duration::from_secs(15),
            None => true,
        };

        if should_log {
            *last_log = Some(now);
        }

        should_log
    }

    #[cfg(test)]
    pub fn call_count(&self) -> usize {
        self.call_count.load(Relaxed)
    }
}

impl StatsSource for ProcessThrottler {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "block_processor",
            "throttled",
            self.cooldown_count.load(Relaxed),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_output_tracker::OutputTrackerMt;
    use tracing_test::traced_test;

    #[test]
    fn dont_wait_when_not_throttling() {
        let (waiter, wait_tracker) = create_fixture(|| false);

        waiter.wait_for_backlog();

        assert_eq!(wait_tracker.output(), vec![]);
    }

    #[test]
    fn wait_when_throttling() {
        let (waiter, wait_tracker) = create_fixture(|| true);

        waiter.wait_for_backlog();

        assert_eq!(wait_tracker.output(), vec![THROTTLE_WAIT]);
    }

    #[test]
    fn stats_source() {
        let (waiter, _) = create_fixture(|| true);

        waiter.wait_for_backlog();

        let mut stats = StatsCollection::new();
        waiter.collect_stats(&mut stats);

        assert_eq!(stats.get("block_processor", "throttled"), 1);
    }

    #[test]
    #[traced_test]
    fn log_initial() {
        let (waiter, _) = create_fixture(|| true);

        waiter.wait_for_backlog();

        logs_assert(|logs| {
            if logs.len() != 1 {
                return Err(format!("len was {}, expected 1", logs.len()));
            }
            if !logs[0].contains("Throttling block processing!") {
                return Err(logs[0].to_owned());
            }
            Ok(())
        });
    }

    #[test]
    #[traced_test]
    fn suppress_logs_for_15_secs() {
        let clock =
            SteadyClock::new_null_with_offsets([Duration::from_secs(14), Duration::from_secs(1)]);
        let queue = Arc::new(BlockProcessorQueue::new_null());
        let waiter = ProcessThrottler::new(queue, clock.into(), || true);

        waiter.wait_for_backlog();
        waiter.wait_for_backlog();
        waiter.wait_for_backlog();

        logs_assert(|logs| {
            if logs.len() != 2 {
                Err(format!("Expected 2 log entries, but found: {}", logs.len()))
            } else {
                Ok(())
            }
        });
    }

    #[test]
    fn can_track_waits() {
        let (waiter, _) = create_fixture(|| true);

        waiter.wait_for_backlog();
        assert_eq!(waiter.call_count(), 1);

        waiter.wait_for_backlog();
        assert_eq!(waiter.call_count(), 2);
    }

    fn create_fixture(
        should_throttle: impl Fn() -> bool + Send + Sync + 'static,
    ) -> (ProcessThrottler, Arc<OutputTrackerMt<Duration>>) {
        let queue = Arc::new(BlockProcessorQueue::new_null());
        let clock = Arc::new(SteadyClock::new_null());
        let wait_tracker = queue.track_waits();
        let waiter = ProcessThrottler::new(queue, clock, should_throttle);
        (waiter, wait_tracker)
    }
}
