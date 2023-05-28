use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(feature = "output_tracking")]
use super::timer::TimerEvent;
#[cfg(feature = "output_tracking")]
use rsnano_core::utils::OutputTrackerMt;

use super::{NullTimer, Timer, TimerStrategy, TimerWrapper};

pub trait ThreadPool: Send + Sync {
    fn push_task(&self, callback: Box<dyn FnMut() + Send>);
    fn add_delayed_task(&self, delay: Duration, callback: Box<dyn FnMut() + Send>);
}

pub struct ThreadPoolImpl<T: TimerStrategy = TimerWrapper> {
    data: Arc<Mutex<Option<ThreadPoolData<T>>>>,
    stopped: Arc<Mutex<bool>>,
}

struct ThreadPoolData<T: TimerStrategy> {
    pool: threadpool::ThreadPool,
    timer: Timer<T>,
}

impl<T: TimerStrategy> ThreadPoolData<T> {
    fn push_task(&self, mut callback: Box<dyn FnMut() + Send>) {
        self.pool.execute(move || callback());
    }
}

impl ThreadPoolImpl<TimerWrapper> {
    pub fn create(num_threads: usize, thread_name: String) -> Self {
        Self::new(num_threads, thread_name, Timer::create())
    }
}

impl ThreadPoolImpl<NullTimer> {
    pub fn create_null() -> Self {
        Self::new(1, "nulled thread pool".to_string(), Timer::create_null())
    }
}

impl<T: TimerStrategy> ThreadPoolImpl<T> {
    pub fn new(num_threads: usize, thread_name: String, timer: Timer<T>) -> Self {
        Self {
            stopped: Arc::new(Mutex::new(false)),
            data: Arc::new(Mutex::new(Some(ThreadPoolData {
                pool: threadpool::Builder::new()
                    .num_threads(num_threads)
                    .thread_name(thread_name)
                    .build(),
                timer,
            }))),
        }
    }

    #[cfg(feature = "output_tracking")]
    pub fn track(&self) -> Arc<OutputTrackerMt<TimerEvent>> {
        self.data.lock().unwrap().as_ref().unwrap().timer.track()
    }

    pub fn stop(&self) {
        let mut stopped_guard = self.stopped.lock().unwrap();
        if !*stopped_guard {
            let mut data_guard = self.data.lock().unwrap();
            *stopped_guard = true;
            drop(stopped_guard);
            if let Some(data) = data_guard.take() {
                data.pool.join();
            }
        }
    }
}

impl<T: TimerStrategy + 'static> ThreadPool for ThreadPoolImpl<T> {
    fn push_task(&self, callback: Box<dyn FnMut() + Send>) {
        let stopped_guard = self.stopped.lock().unwrap();
        if !*stopped_guard {
            let data_guard = self.data.lock().unwrap();
            drop(stopped_guard);
            if let Some(data) = data_guard.as_ref() {
                data.push_task(callback);
            }
        }
    }

    fn add_delayed_task(&self, delay: Duration, callback: Box<dyn FnMut() + Send>) {
        let stopped_guard = self.stopped.lock().unwrap();
        if !*stopped_guard {
            let data_guard = self.data.lock().unwrap();
            drop(stopped_guard);
            let mut option_callback = Some(callback);
            let data_clone = self.data.clone();
            let stopped_clone = self.stopped.clone();
            if let Some(data) = data_guard.as_ref() {
                data.timer.schedule_with_delay(
                    chrono::Duration::from_std(delay).unwrap(),
                    move || {
                        if let Some(cb) = option_callback.take() {
                            let stopped_guard = stopped_clone.lock().unwrap();
                            if !*stopped_guard {
                                let data_guard = data_clone.lock().unwrap();
                                drop(stopped_guard);
                                if let Some(data) = data_guard.as_ref() {
                                    data.push_task(cb);
                                }
                            }
                        }
                    },
                );
            }
        }
    }
}

//todo collect_container_info

impl<T: TimerStrategy> Drop for ThreadPoolImpl<T> {
    fn drop(&mut self) {
        self.stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_task() {
        let (tx, rx) = std::sync::mpsc::channel();
        let pool = ThreadPoolImpl::create(1, "test thread".to_string());
        pool.push_task(Box::new(move || {
            tx.send("foo").unwrap();
        }));
        let result = rx.recv_timeout(Duration::from_millis(300));
        assert_eq!(result, Ok("foo"));
    }

    #[test]
    fn add_delayed_task() {
        let timer = Timer::create_null();
        let timer_tracker = timer.track();
        let pool = ThreadPoolImpl::new(1, "test pool".to_string(), timer);
        let (tx, rx) = std::sync::mpsc::channel();

        pool.add_delayed_task(
            Duration::from_secs(10),
            Box::new(move || {
                tx.send("foo").unwrap();
            }),
        );

        let tasks = timer_tracker.output();
        assert_eq!(tasks.len(), 1, "timer not triggered");
        assert_eq!(tasks[0].delay, chrono::Duration::seconds(10));

        tasks[0].execute_callback();
        let result = rx.recv_timeout(Duration::from_millis(300));
        assert_eq!(result, Ok("foo"));
    }

    #[test]
    fn add_multiple_delayed_tasks() {
        let timer = Timer::create_null();
        let timer_tracker = timer.track();
        let pool = ThreadPoolImpl::new(1, "test pool".to_string(), timer);
        let (tx, rx) = std::sync::mpsc::channel();
        let tx2 = tx.clone();

        pool.add_delayed_task(
            Duration::from_secs(10),
            Box::new(move || {
                tx.send("foo").unwrap();
            }),
        );
        pool.add_delayed_task(
            Duration::from_secs(10),
            Box::new(move || {
                tx2.send("bar").unwrap();
            }),
        );

        let tasks = timer_tracker.output();
        assert_eq!(tasks.len(), 2, "timers not triggered");
        tasks[0].execute_callback();
        tasks[1].execute_callback();
        let result = rx.recv_timeout(Duration::from_millis(300));
        assert_eq!(result, Ok("foo"));
        let result = rx.recv_timeout(Duration::from_millis(300));
        assert_eq!(result, Ok("bar"));
    }

    #[test]
    fn can_be_nulled() {
        let pool = ThreadPoolImpl::create_null();
        let (tx, rx) = std::sync::mpsc::channel();

        let tracker = pool.track();
        pool.add_delayed_task(
            Duration::from_secs(10),
            Box::new(move || {
                tx.send("foo").unwrap();
            }),
        );

        let tasks = tracker.output();
        assert_eq!(tasks.len(), 1, "timer not triggered");
        assert_eq!(tasks[0].delay, chrono::Duration::seconds(10));

        tasks[0].execute_callback();
        let result = rx.recv_timeout(Duration::from_millis(300));
        assert_eq!(result, Ok("foo"));
    }
}