use std::{
    cmp::min,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use rand::RngCore;

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_nullable_random::NullableRngFactory;

use crate::bootstrap::bootstrapper::{
    BootstrapPromise, PollResult, PromiseContext, state::BootstrapLogic,
};

/// Calls a requester to create a bootstrap request and then sends it to
/// the peered node
pub(crate) struct BootstrapPromiseRunner {
    pub state: Arc<Mutex<BootstrapLogic>>,
    pub throttle_wait: Duration,
    pub state_changed: Arc<Condvar>,
    pub clock: Arc<SteadyClock>,
    pub rng_factory: NullableRngFactory,
}

impl BootstrapPromiseRunner {
    const INITIAL_INTERVAL: Duration = Duration::from_millis(5);

    pub fn run<P, T>(&self, mut promise: P) -> Option<T>
    where
        P: BootstrapPromise<T>,
    {
        let mut interval = Self::INITIAL_INTERVAL;
        loop {
            let now = self.clock.now();
            match self.progress(&mut promise, interval, now) {
                Ok(result) => return Some(result),
                Err(i) => interval = i,
            }

            let state = self.state.lock().unwrap();
            if state.stopped {
                return None;
            }
            drop(
                self.state_changed
                    .wait_timeout_while(state, interval, |s| !s.stopped)
                    .unwrap()
                    .0,
            );
        }
    }

    fn progress<A, T>(
        &self,
        action: &mut A,
        mut wait_interval: Duration,
        now: Timestamp,
    ) -> Result<T, Duration>
    where
        A: BootstrapPromise<T>,
    {
        let mut state = self.state.lock().unwrap();
        let mut reset_wait_interval = false;
        let mut poll_count: usize = 0;
        let id = self.rng_factory.rng().next_u64();
        loop {
            poll_count = poll_count.overflowing_add(1).0;

            let mut context = PromiseContext {
                logic: &mut state,
                now,
                id,
            };

            match action.poll(&mut context) {
                PollResult::Progress => {
                    if state.stopped {
                        return Err(Self::INITIAL_INTERVAL);
                    }
                    reset_wait_interval = true;
                    if poll_count.is_multiple_of(100) {
                        drop(state);
                        std::thread::yield_now();
                        state = self.state.lock().unwrap();
                    }
                }
                PollResult::Wait => {
                    wait_interval = if reset_wait_interval {
                        Self::INITIAL_INTERVAL
                    } else {
                        min(wait_interval * 2, self.throttle_wait)
                    };
                    return Err(wait_interval);
                }
                PollResult::Finished(result) => return Ok(result),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_utils::sync::OneShotNotification;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        thread::spawn,
    };

    #[test]
    fn abort_promise_when_stopped() {
        let state = Arc::new(Mutex::new(BootstrapLogic::default()));
        let state_changed = Arc::new(Condvar::new());

        let runner = BootstrapPromiseRunner {
            state: state.clone(),
            throttle_wait: Duration::from_millis(100),
            state_changed: state_changed.clone(),
            clock: SteadyClock::new_null().into(),
            rng_factory: NullableRngFactory::new_null(),
        };

        let promise = StubPromise::new();
        let polled = promise.polled.clone();
        let finished = Arc::new(OneShotNotification::new());
        let finished2 = finished.clone();

        spawn(move || {
            runner.run(promise);
            finished2.notify(());
        });

        polled.wait();
        {
            let mut state = state.lock().unwrap();
            state.stopped = true;
        }
        state_changed.notify_all();
        finished.wait();
    }

    #[test]
    fn return_result_when_finished() {
        let state = Arc::new(Mutex::new(BootstrapLogic::default()));
        let state_changed = Arc::new(Condvar::new());

        let runner = BootstrapPromiseRunner {
            state: state.clone(),
            throttle_wait: Duration::from_millis(100),
            state_changed: state_changed.clone(),
            clock: SteadyClock::new_null().into(),
            rng_factory: NullableRngFactory::new_null(),
        };

        let mut promise = StubPromise::new();
        promise.result = Some(42);

        let result = runner.run(promise);

        assert_eq!(result, Some(42));
    }

    #[test]
    fn pass_random_id() {
        let state = Arc::new(Mutex::new(BootstrapLogic::default()));
        let state_changed = Arc::new(Condvar::new());

        let runner = BootstrapPromiseRunner {
            state: state.clone(),
            throttle_wait: Duration::from_millis(100),
            state_changed: state_changed.clone(),
            clock: SteadyClock::new_null().into(),
            rng_factory: NullableRngFactory::new_null_u64(1234),
        };

        let mut promise = StubPromise::new();
        promise.result = Some(1);
        let last_id = promise.last_id.clone();

        runner.run(promise);

        assert_eq!(last_id.load(Ordering::SeqCst), 1234);
    }

    struct StubPromise {
        polled: Arc<OneShotNotification<()>>,
        result: Option<i32>,
        last_id: Arc<AtomicU64>,
    }

    impl StubPromise {
        fn new() -> Self {
            Self {
                polled: Arc::new(OneShotNotification::new()),
                result: None,
                last_id: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    impl BootstrapPromise<i32> for StubPromise {
        fn poll(&mut self, context: &mut PromiseContext) -> PollResult<i32> {
            self.last_id.store(context.id, Ordering::SeqCst);
            self.polled.notify(());
            if let Some(result) = self.result.take() {
                PollResult::Finished(result)
            } else {
                PollResult::Wait
            }
        }
    }
}
