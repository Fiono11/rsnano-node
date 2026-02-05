pub mod backpressure_channel;

use std::sync::{Arc, Condvar, Mutex};

/// contains (done, Option<result>)
#[derive(Clone)]
pub struct OneShotNotification<T>(Arc<(Mutex<(bool, Option<T>)>, Condvar)>);

impl<T> OneShotNotification<T> {
    pub fn new() -> Self {
        Self(Arc::new((Mutex::new((false, None)), Condvar::new())))
    }

    pub fn notify(&self, t: T) {
        *self.0.0.lock().unwrap() = (true, Some(t));
        self.0.1.notify_one();
    }

    pub fn cancel(&self) {
        *self.0.0.lock().unwrap() = (true, None);
        self.0.1.notify_one();
    }

    pub fn wait(&self) -> Option<T> {
        let guard = self.0.0.lock().unwrap();
        self.0.1.wait_while(guard, |i| !i.0).unwrap().1.take()
    }
}

impl<T> Default for OneShotNotification<T> {
    fn default() -> Self {
        Self::new()
    }
}
