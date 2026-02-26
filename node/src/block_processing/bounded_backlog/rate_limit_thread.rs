use crate::block_processing::bounded_backlog::rate_limit_loop::RateLimitLoop;
use rsnano_utils::{
    CancellationToken,
    thread_factory::{JoinHandle, ThreadFactory},
};

/// Spawns threads that loop with a given rate limit
#[derive(Default)]
pub(crate) struct RateLimitThreadFactory {
    thread_factory: ThreadFactory,
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
        self.thread_factory.spawn(name, move || {
            let scan_loop = RateLimitLoop::new(rate, batch_size);
            scan_loop.run(&cancel_token, f);
        })
    }
}
