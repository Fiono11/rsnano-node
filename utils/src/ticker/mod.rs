mod ticker_pool;
mod timer_thread;

use std::sync::{Arc, Mutex};

use crate::CancellationToken;
pub use ticker_pool::TickerPool;
pub use timer_thread::{TimerStartEvent, TimerStartType, TimerThread};

pub trait Tickable: Send {
    fn tick(&mut self, cancel_token: &CancellationToken);
}

impl<T: Tickable> Tickable for Arc<Mutex<T>> {
    fn tick(&mut self, cancel_token: &CancellationToken) {
        self.lock().unwrap().tick(cancel_token);
    }
}
