use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed},
    },
    time::Duration,
};

use rsnano_utils::stats::{StatsCollection, StatsSource};

const MIN_THROTTLE_WAIT_MS: u64 = 100;
const MAX_THROTTLE_WAIT_MS: u64 = 1000;

/// Decides whether to throttle and by how much.
/// The wait duration starts at 100 ms and doubles with each consecutive throttle,
/// up to a maximum of 1 second. It resets to the minimum when not throttling.
/// Returns `Some(duration)` when throttling is needed, `None` otherwise.
pub(crate) struct ProcessThrottler {
    should_throttle: Mutex<Arc<dyn Fn() -> bool + Send + Sync>>,
    cooldown_count: AtomicUsize,
    current_wait_ms: AtomicU64,
}

impl ProcessThrottler {
    pub fn new(should_throttle: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            should_throttle: Mutex::new(should_throttle),
            cooldown_count: AtomicUsize::new(0),
            current_wait_ms: AtomicU64::new(MIN_THROTTLE_WAIT_MS),
        }
    }

    pub fn set_should_throttle(&self, callback: Arc<dyn Fn() -> bool + Send + Sync>) {
        *self.should_throttle.lock().unwrap() = callback;
    }

    /// Returns `Some(duration)` if throttling is needed, `None` otherwise.
    pub fn throttle(&self) -> Option<Duration> {
        if !(self.should_throttle.lock().unwrap())() {
            self.current_wait_ms.store(MIN_THROTTLE_WAIT_MS, Relaxed);
            return None;
        }

        let wait_ms = self.current_wait_ms.load(Relaxed);
        let next_wait_ms = (wait_ms * 2).min(MAX_THROTTLE_WAIT_MS);
        self.current_wait_ms.store(next_wait_ms, Relaxed);
        self.cooldown_count.fetch_add(1, Relaxed);
        Some(Duration::from_millis(wait_ms))
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
    use std::sync::atomic::AtomicBool;

    #[test]
    fn dont_wait_when_not_throttling() {
        let waiter = create_fixture(|| false);

        assert!(waiter.throttle().is_none());
    }

    #[test]
    fn wait_when_throttling() {
        let waiter = create_fixture(|| true);

        assert_eq!(
            waiter.throttle(),
            Some(Duration::from_millis(MIN_THROTTLE_WAIT_MS))
        );
    }

    #[test]
    fn wait_doubles_on_consecutive_throttles() {
        let waiter = create_fixture(|| true);

        assert_eq!(waiter.throttle(), Some(Duration::from_millis(100)));
        assert_eq!(waiter.throttle(), Some(Duration::from_millis(200)));
        assert_eq!(waiter.throttle(), Some(Duration::from_millis(400)));
    }

    #[test]
    fn wait_is_capped_at_max() {
        let waiter = create_fixture(|| true);

        let mut last = None;
        for _ in 0..20 {
            last = waiter.throttle();
        }

        assert_eq!(last, Some(Duration::from_millis(MAX_THROTTLE_WAIT_MS)));
    }

    #[test]
    fn wait_resets_after_not_throttling() {
        let throttle = Arc::new(AtomicBool::new(true));
        let throttle2 = throttle.clone();

        let waiter = ProcessThrottler::new(Arc::new(move || {
            let v = throttle2.load(Relaxed);
            throttle2.store(!v, Relaxed);
            v
        }));

        assert_eq!(waiter.throttle(), Some(Duration::from_millis(100))); // throttled: 100 ms, next = 200
        assert!(waiter.throttle().is_none()); // not throttled: reset to 100, no wait
        assert_eq!(waiter.throttle(), Some(Duration::from_millis(100))); // throttled again: 100 ms
    }

    #[test]
    fn stats_source() {
        let waiter = create_fixture(|| true);

        waiter.throttle();

        let mut stats = StatsCollection::new();
        waiter.collect_stats(&mut stats);

        assert_eq!(stats.get("block_processor", "throttled"), 1);
    }

    /* Test helpers */

    fn create_fixture(
        should_throttle: impl Fn() -> bool + Send + Sync + 'static,
    ) -> ProcessThrottler {
        ProcessThrottler::new(Arc::new(should_throttle))
    }
}
