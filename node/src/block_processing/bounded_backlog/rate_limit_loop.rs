use std::time::Duration;

use rsnano_network::token_bucket::TokenBucket;
use rsnano_nullable_clock::SteadyClock;
use rsnano_utils::CancellationToken;

/// Calls a function in a loop with a given rate limit
pub(crate) struct RateLimitLoop {
    clock: SteadyClock,
    limiter: TokenBucket,
    batch_size: usize,
}

impl RateLimitLoop {
    pub(crate) fn new(rate: usize, batch_size: usize) -> Self {
        Self {
            clock: SteadyClock::default(),
            limiter: TokenBucket::new(rate),
            batch_size,
        }
    }

    pub(crate) fn run<F>(mut self, cancel_token: &CancellationToken, mut f: F)
    where
        F: FnMut(),
    {
        while !cancel_token.is_cancelled() {
            self.wait_limiter(&cancel_token);

            if cancel_token.is_cancelled() {
                return;
            }

            (f)();
        }
    }

    fn wait_limiter(&mut self, cancel_token: &CancellationToken) {
        while !self.limiter.try_consume(self.batch_size, self.clock.now()) {
            if cancel_token.wait_for_cancellation(Duration::from_millis(100)) {
                break;
            }
        }
    }
}
