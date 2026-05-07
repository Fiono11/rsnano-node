use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicIsize, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

use rsnano_utils::{
    EventHandlerMut,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

pub(crate) struct EventProcessor {
    queue_thread: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<EventProcessorStats>,
}

impl EventProcessor {
    pub fn new(queue_name: &'static str) -> Self {
        Self {
            queue_thread: Mutex::new(None),
            stats: Arc::new(EventProcessorStats {
                enqueued: AtomicU64::new(0),
                overfill: AtomicU64::new(0),
                process_duration: AtomicU64::new(0),
                queue_len: AtomicIsize::new(0),
                queue_name,
            }),
        }
    }

    pub fn start<T: Send + 'static>(
        &self,
        thread_name: impl Into<String>,
        max_queue: usize,
        mut handle: impl EventHandlerMut<T> + 'static,
    ) -> EventSender<T> {
        let stats = self.stats.clone();
        let (tx_event, rx_event) = mpsc::sync_channel::<T>(max_queue);

        let ev_processor_thread = std::thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                while let Ok(i) = rx_event.recv() {
                    stats.queue_len.fetch_sub(1, Ordering::Relaxed);
                    let start = std::time::Instant::now();
                    handle.handle(&i);
                    let elapsed = start.elapsed().as_micros() as u64;
                    stats.process_duration.fetch_add(elapsed, Ordering::Relaxed);
                }
            })
            .unwrap();

        *self.queue_thread.lock().unwrap() = Some(ev_processor_thread);
        EventSender::new(tx_event, self.stats.clone())
    }

    pub fn join(&self) {
        if let Some(handle) = self.queue_thread.lock().unwrap().take() {
            handle.join().expect("thread should join");
        }
    }
}

impl ContainerInfoProvider for EventProcessor {
    fn container_info(&self) -> ContainerInfo {
        let count = self.stats.queue_len.load(Ordering::Relaxed).max(0);
        ContainerInfo::builder()
            .leaf(self.stats.queue_name, count as usize, 0)
            .finish()
    }
}

impl StatsSource for EventProcessor {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result);
    }
}

pub(crate) struct EventSender<T> {
    tx: mpsc::SyncSender<T>,
    stats: Arc<EventProcessorStats>,
}

impl<T> EventSender<T> {
    fn new(tx: mpsc::SyncSender<T>, stats: Arc<EventProcessorStats>) -> Self {
        Self { tx, stats }
    }

    pub fn try_send(&self, event: T) -> bool {
        match self.tx.try_send(event) {
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
}

struct EventProcessorStats {
    enqueued: AtomicU64,
    overfill: AtomicU64,
    process_duration: AtomicU64,
    queue_len: AtomicIsize,
    queue_name: &'static str,
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
