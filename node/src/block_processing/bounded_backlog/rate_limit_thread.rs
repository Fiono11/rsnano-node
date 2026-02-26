use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_network::token_bucket::TokenBucket;
use rsnano_nullable_clock::SteadyClock;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use rsnano_utils::{
    CancellationToken,
    thread_factory::{JoinHandle, ThreadFactory},
};

/// Spawns threads that loop with a given rate limit
#[derive(Default)]
pub(crate) struct RateLimitThreadFactory {
    thread_factory: ThreadFactory,
    scan_loop: RateLimitLoop,
    spawn_listener: OutputListenerMt<SpawnEvent>,
}

impl RateLimitThreadFactory {
    pub fn spawn<F>(
        &self,
        name: impl Into<String>,
        cancel_token: CancellationToken,
        rate: usize,
        batch_size: usize,
        f: F,
    ) -> JoinHandle
    where
        F: FnMut() + Send + 'static,
    {
        let callback: Option<Box<dyn FnMut() + Send>> = Some(Box::new(f));
        let callback = Arc::new(Mutex::new(callback));

        let name = name.into();
        self.spawn_listener.emit(SpawnEvent {
            thread_name: name.clone(),
            callback: callback.clone(),
        });
        let scan_loop = self.scan_loop.clone();
        self.thread_factory.spawn(name, move || {
            if let Some(callback_func) = callback.lock().unwrap().take() {
                scan_loop.run(rate, batch_size, &cancel_token, callback_func);
            }
        })
    }

    #[allow(dead_code)]
    pub fn track_spawns(&self) -> Arc<OutputTrackerMt<SpawnEvent>> {
        self.spawn_listener.track()
    }
}

/// Calls a function in a loop with a given rate limit
#[derive(Clone, Default)]
struct RateLimitLoop {
    clock: SteadyClock,
    limiter: TokenBucket,
}

impl RateLimitLoop {
    fn new(clock: SteadyClock) -> Self {
        Self {
            clock,
            limiter: Default::default(),
        }
    }

    #[allow(dead_code)]
    fn new_null() -> Self {
        Self::new(SteadyClock::new_null())
    }

    fn run<F>(mut self, rate: usize, batch_size: usize, cancel_token: &CancellationToken, mut f: F)
    where
        F: FnMut() + Send + 'static,
    {
        self.limiter.reset_with(rate, rate);

        while !cancel_token.is_cancelled() {
            self.wait_limiter(batch_size, &cancel_token);

            if cancel_token.is_cancelled() {
                return;
            }

            (f)();
        }
    }

    fn wait_limiter(&mut self, batch_size: usize, cancel_token: &CancellationToken) {
        while !self.limiter.try_consume(batch_size, self.clock.now()) {
            if cancel_token.wait_for_cancellation(Duration::from_millis(100)) {
                break;
            }
        }
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct SpawnEvent {
    pub(crate) thread_name: String,
    pub(crate) callback: Arc<Mutex<Option<Box<dyn FnMut() + Send>>>>,
}

impl SpawnEvent {
    #[allow(dead_code)]
    pub fn run(&self) {
        let Some(mut callback) = self.callback.lock().unwrap().take() else {
            panic!("No callback to call");
        };
        callback();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn spawns_thread() {
        let factory = RateLimitThreadFactory {
            thread_factory: ThreadFactory::new_null(),
            scan_loop: RateLimitLoop::new_null(),
            spawn_listener: Default::default(),
        };

        let spawn_tracker = factory.thread_factory.track_spawns();
        let cancel_token = CancellationToken::new_null_with_uncancelled_waits(1);
        let call_counter = Arc::new(AtomicUsize::new(0));
        let call_counter2 = call_counter.clone();
        let _ = factory.spawn("testthread", cancel_token, 1, 1, move || {
            call_counter2.fetch_add(1, Ordering::Relaxed);
        });

        let spawn = spawn_tracker
            .output()
            .pop()
            .expect("should have spawned thread");

        assert_eq!(spawn.thread_name, "testthread");
        assert_eq!(call_counter.load(Ordering::Relaxed), 0);

        spawn.run();
        assert_eq!(call_counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn loops_until_limiter_kicks_in() {
        let factory = RateLimitThreadFactory {
            thread_factory: ThreadFactory::new_null(),
            scan_loop: RateLimitLoop::new_null(),
            spawn_listener: Default::default(),
        };

        let spawn_tracker = factory.thread_factory.track_spawns();
        let cancel_token = CancellationToken::new_null_with_uncancelled_waits(1);
        let call_counter = Arc::new(AtomicUsize::new(0));
        let call_counter2 = call_counter.clone();
        let _ = factory.spawn("testthread", cancel_token, 3, 1, move || {
            call_counter2.fetch_add(1, Ordering::Relaxed);
        });

        let spawn = spawn_tracker
            .output()
            .pop()
            .expect("should have spawned thread");

        assert_eq!(spawn.thread_name, "testthread");
        assert_eq!(call_counter.load(Ordering::Relaxed), 0);

        spawn.run();
        assert_eq!(call_counter.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn spawns_can_be_tracked() {
        let factory = RateLimitThreadFactory {
            thread_factory: ThreadFactory::new_null(),
            scan_loop: RateLimitLoop::new_null(),
            spawn_listener: Default::default(),
        };

        let spawn_tracker = factory.track_spawns();
        let cancel_token = CancellationToken::new_null();
        let call_counter = Arc::new(AtomicUsize::new(0));
        let call_counter2 = call_counter.clone();
        let _ = factory.spawn("testthread", cancel_token, 1, 1, move || {
            call_counter2.fetch_add(1, Ordering::Relaxed);
        });

        let spawn = spawn_tracker
            .output()
            .pop()
            .expect("should have spawned thread");

        assert_eq!(spawn.thread_name, "testthread");
        assert_eq!(call_counter.load(Ordering::Relaxed), 0);
        spawn.run();
        assert_eq!(call_counter.load(Ordering::Relaxed), 1);
    }
}
