use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed},
    },
    time::Duration,
};

use tracing::warn;

use rsnano_nullable_clock::{SteadyClock, Timestamp};

use super::BlockProcessorQueue;
use rsnano_utils::stats::{StatsCollection, StatsSource};

const MIN_THROTTLE_WAIT_MS: u64 = 100;
const MAX_THROTTLE_WAIT_MS: u64 = 1000;

/// Waits until a condition is met (the `should_throttle` callback returns false).
/// The wait duration starts at 100 ms and doubles with each consecutive throttle,
/// up to a maximum of 1 second. It resets to the minimum when not throttling.
pub(crate) struct ProcessThrottler {
    queue: Arc<BlockProcessorQueue>,
    should_throttle: Arc<dyn Fn() -> bool + Send + Sync>,
    call_count: AtomicUsize,
    cooldown_count: AtomicUsize,
    last_log: Mutex<Option<Timestamp>>,
    clock: Arc<SteadyClock>,
    current_wait_ms: AtomicU64,
}

impl ProcessThrottler {
    pub fn new(
        queue: Arc<BlockProcessorQueue>,
        clock: Arc<SteadyClock>,
        should_throttle: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            queue,
            should_throttle,
            call_count: AtomicUsize::new(0),
            cooldown_count: AtomicUsize::new(0),
            last_log: Mutex::new(None),
            clock,
            current_wait_ms: AtomicU64::new(MIN_THROTTLE_WAIT_MS),
        }
    }

    #[cfg(test)]
    pub fn new_null() -> Self {
        let queue = Arc::new(BlockProcessorQueue::new_null());
        let clock = Arc::new(SteadyClock::new_null());
        Self::new(queue, clock, Arc::new(|| false))
    }

    pub fn throttle(&self) {
        self.call_count.fetch_add(1, Relaxed);
        if !(self.should_throttle)() {
            self.current_wait_ms.store(MIN_THROTTLE_WAIT_MS, Relaxed);
            return;
        }

        let now = self.clock.now();
        if self.should_log(now) {
            warn!("Throttling block processing!");
        }

        let wait_ms = self.current_wait_ms.load(Relaxed);
        let next_wait_ms = (wait_ms * 2).min(MAX_THROTTLE_WAIT_MS);
        self.current_wait_ms.store(next_wait_ms, Relaxed);
        self.cooldown_count.fetch_add(1, Relaxed);
        self.queue.wait(Duration::from_millis(wait_ms));
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

        waiter.throttle();

        assert_eq!(wait_tracker.output(), vec![]);
    }

    #[test]
    fn wait_when_throttling() {
        let (waiter, wait_tracker) = create_fixture(|| true);

        waiter.throttle();

        assert_eq!(
            wait_tracker.output(),
            vec![Duration::from_millis(MIN_THROTTLE_WAIT_MS)]
        );
    }

    #[test]
    fn wait_doubles_on_consecutive_throttles() {
        let (waiter, wait_tracker) = create_fixture(|| true);

        waiter.throttle(); // 100 ms
        waiter.throttle(); // 200 ms
        waiter.throttle(); // 400 ms

        assert_eq!(
            wait_tracker.output(),
            vec![
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(400),
            ]
        );
    }

    #[test]
    fn wait_is_capped_at_max() {
        let (waiter, wait_tracker) = create_fixture(|| true);

        for _ in 0..20 {
            waiter.throttle();
        }

        let waits = wait_tracker.output();
        assert_eq!(
            *waits.last().unwrap(),
            Duration::from_millis(MAX_THROTTLE_WAIT_MS)
        );
    }

    #[test]
    fn wait_resets_after_not_throttling() {
        use std::sync::atomic::AtomicBool;
        let throttle = Arc::new(AtomicBool::new(true));
        let throttle2 = throttle.clone();

        let queue = Arc::new(BlockProcessorQueue::new_null());
        let clock = Arc::new(SteadyClock::new_null());
        let wait_tracker = queue.track_waits();
        let waiter = ProcessThrottler::new(
            queue,
            clock,
            Arc::new(move || {
                let v = throttle2.load(Relaxed);
                throttle2.store(!v, Relaxed);
                v
            }),
        );

        waiter.throttle(); // throttled: 100 ms, next = 200
        waiter.throttle(); // not throttled: reset to 100, no wait
        waiter.throttle(); // throttled again: 100 ms

        assert_eq!(
            wait_tracker.output(),
            vec![Duration::from_millis(100), Duration::from_millis(100)]
        );
    }

    #[test]
    fn stats_source() {
        let (waiter, _) = create_fixture(|| true);

        waiter.throttle();

        let mut stats = StatsCollection::new();
        waiter.collect_stats(&mut stats);

        assert_eq!(stats.get("block_processor", "throttled"), 1);
    }

    #[test]
    #[traced_test]
    fn log_initial() {
        let (waiter, _) = create_fixture(|| true);

        waiter.throttle();

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
        let waiter = ProcessThrottler::new(queue, clock.into(), Arc::new(|| true));

        waiter.throttle();
        waiter.throttle();
        waiter.throttle();

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

        waiter.throttle();
        assert_eq!(waiter.call_count(), 1);

        waiter.throttle();
        assert_eq!(waiter.call_count(), 2);
    }

    fn create_fixture(
        should_throttle: impl Fn() -> bool + Send + Sync + 'static,
    ) -> (ProcessThrottler, Arc<OutputTrackerMt<Duration>>) {
        let queue = Arc::new(BlockProcessorQueue::new_null());
        let clock = Arc::new(SteadyClock::new_null());
        let wait_tracker = queue.track_waits();
        let waiter = ProcessThrottler::new(queue, clock, Arc::new(should_throttle));
        (waiter, wait_tracker)
    }
}
