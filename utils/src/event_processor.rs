use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicIsize, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

use crate::{
    EventHandlerMut,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};

pub struct EventProcessor<T: Clone + Send + 'static> {
    rx: Mutex<Option<mpsc::Receiver<T>>>,
    queue_thread: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<EventProcessorStats>,
}

impl<T: Clone + Send + 'static> EventProcessor<T> {
    pub fn new(queue_name: &'static str, max_queue: usize) -> (Self, EventSender<T>) {
        let (tx, rx) = mpsc::sync_channel(max_queue);
        let stats = Arc::new(EventProcessorStats::new(queue_name));
        let listener = Arc::new(OutputListenerMt::new());

        let processor = Self {
            rx: Mutex::new(Some(rx)),
            queue_thread: Mutex::new(None),
            stats: stats.clone(),
        };

        let sender = EventSender {
            tx: Some(tx),
            stats,
            listener,
        };

        (processor, sender)
    }

    pub fn start(
        &self,
        thread_name: impl Into<String>,
        mut handle: impl EventHandlerMut<T> + 'static,
    ) {
        let rx = self
            .rx
            .lock()
            .unwrap()
            .take()
            .expect("event processor already started");
        let stats = self.stats.clone();
        let thread = std::thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                while let Ok(i) = rx.recv() {
                    stats.queue_len.fetch_sub(1, Ordering::Relaxed);
                    let start = std::time::Instant::now();
                    handle.handle(&i);
                    let elapsed = start.elapsed().as_micros() as u64;
                    stats.process_duration.fetch_add(elapsed, Ordering::Relaxed);
                }
            })
            .unwrap();
        *self.queue_thread.lock().unwrap() = Some(thread);
    }

    pub fn join(&self) {
        if let Some(handle) = self.queue_thread.lock().unwrap().take() {
            handle.join().expect("thread should join");
        }
    }
}

impl<T: Clone + Send + 'static> ContainerInfoProvider for EventProcessor<T> {
    fn container_info(&self) -> ContainerInfo {
        let count = self.stats.queue_len.load(Ordering::Relaxed).max(0);
        ContainerInfo::builder()
            .leaf(self.stats.queue_name, count as usize, 0)
            .finish()
    }
}

impl<T: Clone + Send + 'static> StatsSource for EventProcessor<T> {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result);
    }
}

pub struct EventSender<T: Clone + 'static> {
    tx: Option<mpsc::SyncSender<T>>,
    stats: Arc<EventProcessorStats>,
    listener: Arc<OutputListenerMt<T>>,
}

impl<T: Clone + 'static> EventSender<T> {
    pub fn new_null() -> Self {
        Self {
            tx: None,
            stats: Arc::new(EventProcessorStats::new("null")),
            listener: Arc::new(OutputListenerMt::new()),
        }
    }

    pub fn track(&self) -> Arc<OutputTrackerMt<T>> {
        self.listener.track()
    }

    /// Non-blocking send. Returns false if the queue was full (event dropped).
    /// The event is always emitted to the listener regardless of the tx state.
    pub fn try_send(&self, event: T) -> bool {
        self.listener.emit(event.clone());
        let Some(tx) = &self.tx else { return true };
        match tx.try_send(event) {
            Ok(()) => {
                self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
                self.stats.queue_len.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.stats.overfill.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                unreachable!("queue should be open")
            }
        }
    }

    /// Blocking send. Blocks until the consumer drains enough to make room.
    pub fn send(&self, event: T) {
        self.listener.emit(event.clone());
        let Some(tx) = &self.tx else { return };
        if tx.send(event).is_err() {
            unreachable!("queue should be open")
        }
        self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
        self.stats.queue_len.fetch_add(1, Ordering::Relaxed);
    }
}

impl<T: Clone + 'static> Clone for EventSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            stats: self.stats.clone(),
            listener: self.listener.clone(),
        }
    }
}

struct EventProcessorStats {
    enqueued: AtomicU64,
    overfill: AtomicU64,
    process_duration: AtomicU64,
    queue_len: AtomicIsize,
    queue_name: &'static str,
}

impl EventProcessorStats {
    fn new(queue_name: &'static str) -> Self {
        Self {
            enqueued: AtomicU64::new(0),
            overfill: AtomicU64::new(0),
            process_duration: AtomicU64::new(0),
            queue_len: AtomicIsize::new(0),
            queue_name,
        }
    }
}

impl StatsSource for EventProcessorStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            self.queue_name,
            "enqueued",
            self.enqueued.load(Ordering::Relaxed),
        );
        result.insert(
            self.queue_name,
            "overfill",
            self.overfill.load(Ordering::Relaxed),
        );
        result.insert(
            "ev_proc_duration",
            self.queue_name,
            self.process_duration.load(Ordering::Relaxed),
        );
    }
}
