use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::Duration,
};

#[derive(Clone, PartialEq, Debug)]
pub enum NotifyEvent {
    NotifyOne,
    NotifyAll,
}

pub struct NullableCondvarMutex<T> {
    strategy: CondvarStrategy<T>,
    mutex: Mutex<T>,
    notify_listener: OutputListenerMt<NotifyEvent>,
}

impl<T> NullableCondvarMutex<T> {
    pub fn new(t: T) -> Self {
        Self {
            strategy: CondvarStrategy::Real(Condvar::new()),
            mutex: Mutex::new(t),
            notify_listener: OutputListenerMt::new(),
        }
    }

    pub fn new_null(t: T) -> Self {
        Self {
            strategy: CondvarStrategy::Nulled(Default::default()),
            mutex: Mutex::new(t),
            notify_listener: OutputListenerMt::new(),
        }
    }

    pub fn null_builder(t: T) -> NullableCondvarMutexBuilder<T> {
        NullableCondvarMutexBuilder { t, callbacks: None }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.mutex.lock().unwrap()
    }

    pub fn notify_one(&self) {
        self.notify_listener.emit(NotifyEvent::NotifyOne);
        match &self.strategy {
            CondvarStrategy::Real(condvar) => condvar.notify_one(),
            CondvarStrategy::Nulled(_) => {}
        }
    }

    pub fn notify_all(&self) {
        self.notify_listener.emit(NotifyEvent::NotifyAll);
        match &self.strategy {
            CondvarStrategy::Real(condvar) => condvar.notify_all(),
            CondvarStrategy::Nulled(_) => {}
        }
    }

    pub fn track_notifications(&self) -> Arc<OutputTrackerMt<NotifyEvent>> {
        self.notify_listener.track()
    }

    pub fn wait<'a>(&'a self, mut guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        match &self.strategy {
            CondvarStrategy::Real(condvar) => condvar.wait(guard).unwrap(),
            CondvarStrategy::Nulled(condvar) => {
                condvar.execute_wait_callback(&mut guard);
                guard
            }
        }
    }

    pub fn wait_while<'a, F>(
        &'a self,
        mut guard: MutexGuard<'a, T>,
        mut condition: F,
    ) -> MutexGuard<'a, T>
    where
        F: FnMut(&mut T) -> bool,
    {
        match &self.strategy {
            CondvarStrategy::Real(condvar) => condvar.wait_while(guard, condition).unwrap(),
            CondvarStrategy::Nulled(condvar) => {
                condvar.execute_wait_callback(&mut guard);
                if condition(&mut guard) {
                    panic!("wait_while called on nulled condvar, but condition met");
                }
                guard
            }
        }
    }

    pub fn wait_timeout<'a>(
        &'a self,
        mut guard: MutexGuard<'a, T>,
        dur: Duration,
    ) -> (MutexGuard<'a, T>, bool) {
        match &self.strategy {
            CondvarStrategy::Real(condvar) => {
                let (guard, timeout) = condvar.wait_timeout(guard, dur).unwrap();
                (guard, timeout.timed_out())
            }
            CondvarStrategy::Nulled(condvar) => {
                condvar.execute_wait_callback(&mut guard);
                (guard, false)
            }
        }
    }

    pub fn wait_timeout_while<'a, F>(
        &'a self,
        mut guard: MutexGuard<'a, T>,
        dur: Duration,
        mut condition: F,
    ) -> (MutexGuard<'a, T>, bool)
    where
        F: FnMut(&mut T) -> bool,
    {
        match &self.strategy {
            CondvarStrategy::Real(condvar) => {
                let (guard, timeout) = condvar.wait_timeout_while(guard, dur, condition).unwrap();
                (guard, timeout.timed_out())
            }
            CondvarStrategy::Nulled(condvar) => {
                condvar.execute_wait_callback(&mut guard);
                let timed_out = condition(&mut guard);
                (guard, timed_out)
            }
        }
    }
}

type WaitCallback<T> = Box<dyn FnOnce(&mut T) + Send>;

enum CondvarStrategy<T> {
    Real(Condvar),
    Nulled(StubCondvar<T>),
}

struct StubCondvar<T> {
    wait_callbacks: Mutex<Option<Vec<WaitCallback<T>>>>,
}

impl<T> StubCondvar<T> {
    fn new(wait_callbacks: Option<Vec<WaitCallback<T>>>) -> Self {
        Self {
            wait_callbacks: Mutex::new(wait_callbacks),
        }
    }

    fn execute_wait_callback(&self, data: &mut T) {
        let mut callbacks_guard = self.wait_callbacks.lock().unwrap();
        if let Some(callbacks) = &mut *callbacks_guard {
            let Some(cb) = callbacks.pop() else {
                panic!("wait called unexpectedly on nulled condvar");
            };
            drop(callbacks_guard);
            cb(data);
        }
    }
}

impl<T> Default for StubCondvar<T> {
    fn default() -> Self {
        Self {
            wait_callbacks: Mutex::new(None),
        }
    }
}

pub struct NullableCondvarMutexBuilder<T> {
    t: T,
    callbacks: Option<Vec<WaitCallback<T>>>,
}

impl<T> NullableCondvarMutexBuilder<T> {
    pub fn wait<F>(mut self, callback: F) -> Self
    where
        F: FnOnce(&mut T) + Send + 'static,
    {
        self.callbacks
            .get_or_insert_default()
            .insert(0, Box::new(callback));
        self
    }

    pub fn finish(self) -> NullableCondvarMutex<T> {
        NullableCondvarMutex {
            strategy: CondvarStrategy::Nulled(StubCondvar::new(self.callbacks)),
            mutex: Mutex::new(self.t),
            notify_listener: OutputListenerMt::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /*
     * Real implementation
     */

    #[test]
    fn delegates_to_real_condvar() {
        let mutex = Arc::new(NullableCondvarMutex::new(false));
        let mutex1 = mutex.clone();
        let guard = mutex.lock();
        std::thread::spawn(move || {
            let mut g = mutex1.lock();
            *g = true;
            mutex1.notify_all();
        });
        let (guard, timeout) =
            mutex.wait_timeout_while(guard, Duration::from_secs(5), |ready| !*ready);
        assert!(!timeout);
        assert!(*guard);
    }

    #[test]
    fn wait_while() {
        let mutex = Arc::new(Mutex::new(false));
        let mutex1 = mutex.clone();
        let condvar = Arc::new(NullableCondvarMutex::new(false));
        let condvar1 = condvar.clone();
        let guard = mutex.lock().unwrap();
        std::thread::spawn(move || {
            condvar1.notify_all();
            *mutex1.lock().unwrap() = true;
            condvar1.notify_all()
        });
        let guard = condvar.wait_while(guard, |g| !*g);
        assert!(*guard);
    }

    /*
     * Nullability
     */

    #[test]
    fn nulled_condvar_returns_immediately_when_wait_called() {
        let mutex = NullableCondvarMutex::new_null(0);
        let mut guard = mutex.lock();
        guard = mutex.wait(guard);
        assert_eq!(*guard, 0);
    }

    #[test]
    fn nulled_condvar_returns_immediately_when_wait_while_called_and_condition_not_met() {
        let mutex = NullableCondvarMutex::new_null(1);
        let mut guard = mutex.lock();
        guard = mutex.wait_while(guard, |g| *g != 1);
        assert_eq!(*guard, 1);
    }

    #[test]
    #[should_panic = "wait_while called on nulled condvar, but condition met"]
    fn nulled_condvar_panics_when_wait_while_called_and_condition_met() {
        let mutex = NullableCondvarMutex::new_null(1);
        let guard = mutex.lock();
        drop(mutex.wait_while(guard, |g| *g == 1));
    }

    #[test]
    fn nulled_condvar_executes_configured_wait_callback() {
        let mutex = NullableCondvarMutex::<usize>::null_builder(1)
            .wait(|i| *i = 2)
            .finish();
        let mut guard = mutex.lock();
        guard = mutex.wait_while(guard, |g| *g == 1);
        assert_eq!(*guard, 2);
    }

    #[test]
    #[should_panic = "wait called unexpectedly on nulled condvar"]
    fn nulled_condvar_panics_when_wait_called_more_often_than_configured() {
        let mutex = NullableCondvarMutex::<usize>::null_builder(1)
            .wait(|_| {})
            .finish();
        let mut guard = mutex.lock();
        guard = mutex.wait(guard);
        guard = mutex.wait(guard);
        drop(guard);
    }

    #[test]
    fn nulled_condvar_returns_immediately_when_wait_timeout_called() {
        let mutex = NullableCondvarMutex::new_null(0);
        let mut guard = mutex.lock();
        guard = mutex.wait_timeout(guard, Duration::from_secs(5)).0;
        guard = mutex.wait_timeout(guard, Duration::from_secs(5)).0;
        assert_eq!(*guard, 0);
    }

    #[test]
    fn nulled_condvar_executes_configured_callback_when_wait_timeout_called() {
        let mutex = NullableCondvarMutex::null_builder(0)
            .wait(|i| *i = 1)
            .finish();
        let mut guard = mutex.lock();
        guard = mutex.wait_timeout(guard, Duration::from_secs(5)).0;
        assert_eq!(*guard, 1);
    }

    #[test]
    fn nulled_condvar_returns_timeout_when_wait_timeout_while_called_and_condition_met() {
        let mutex = NullableCondvarMutex::new_null(1);
        let guard = mutex.lock();
        let (_unused, timeout) =
            mutex.wait_timeout_while(guard, Duration::from_secs(5), |g| *g == 1);
        assert!(timeout)
    }

    #[test]
    fn nulled_condvar_allows_multiple_calls_to_wait_timeout_while() {
        let mutex = NullableCondvarMutex::null_builder(0)
            .wait(|i| *i = 1)
            .wait(|i| *i = 2)
            .finish();

        let mut guard = mutex.lock();
        let mut timeout;

        (guard, timeout) = mutex.wait_timeout_while(guard, Duration::from_secs(10), |_| true);
        assert!(timeout);
        assert_eq!(*guard, 1);

        (guard, timeout) = mutex.wait_timeout_while(guard, Duration::from_secs(10), |_| true);
        assert!(timeout);
        assert_eq!(*guard, 2);
    }

    /*
     * Notification tracking
     */

    #[test]
    fn tracks_notify_one() {
        let mutex = NullableCondvarMutex::new(0);
        let tracker = mutex.track_notifications();
        mutex.notify_one();
        assert_eq!(tracker.output(), vec![NotifyEvent::NotifyOne]);
    }

    #[test]
    fn tracks_notify_all() {
        let mutex = NullableCondvarMutex::new(0);
        let tracker = mutex.track_notifications();
        mutex.notify_all();
        assert_eq!(tracker.output(), vec![NotifyEvent::NotifyAll]);
    }

    #[test]
    fn tracks_multiple_notifications() {
        let mutex = NullableCondvarMutex::new(0);
        let tracker = mutex.track_notifications();
        mutex.notify_one();
        mutex.notify_all();
        mutex.notify_one();
        assert_eq!(
            tracker.output(),
            vec![
                NotifyEvent::NotifyOne,
                NotifyEvent::NotifyAll,
                NotifyEvent::NotifyOne,
            ]
        );
    }

    #[test]
    fn tracking_works_on_nulled_condvar() {
        let mutex = NullableCondvarMutex::new_null(0);
        let tracker = mutex.track_notifications();
        mutex.notify_one();
        mutex.notify_all();
        assert_eq!(
            tracker.output(),
            vec![NotifyEvent::NotifyOne, NotifyEvent::NotifyAll]
        );
    }
}
