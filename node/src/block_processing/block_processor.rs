use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};

use tracing::warn;

use rsnano_ledger::Ledger;
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_utils::stats::{StatsCollection, StatsSource};

use super::{
    BlockProcessorQueue, UncheckedBlockReenqueuer, UncheckedMap,
    block_batch_processor::BlockBatchProcessorStats,
};
use crate::block_processing::{
    BlockContext, block_batch_processor::BlockBatchProcessor, process_throttler::ProcessThrottler,
};

pub struct BlockProcessor {
    threads: Mutex<Vec<JoinHandle<()>>>,
    process_queue: Arc<BlockProcessorQueue>,
    ledger: Arc<Ledger>,
    unchecked: Arc<Mutex<UncheckedMap>>,
    process_stats: Arc<BlockBatchProcessorStats>,
    unchecked_reenqueuer: UncheckedBlockReenqueuer,
    clock: Arc<SteadyClock>,
    throttler: Arc<ProcessThrottler>,
}

impl BlockProcessor {
    pub(crate) fn new(
        process_queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        unchecked: Arc<Mutex<UncheckedMap>>,
        unchecked_reenqueuer: UncheckedBlockReenqueuer,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            process_queue,
            ledger,
            unchecked,
            unchecked_reenqueuer,
            process_stats: Arc::new(BlockBatchProcessorStats::default()),
            threads: Mutex::new(Vec::new()),
            clock,
            throttler: ProcessThrottler::new(Arc::new(|| false)).into(),
        }
    }

    pub fn set_should_throttle(&self, callback: Arc<dyn Fn() -> bool + Send + Sync>) {
        self.throttler.set_should_throttle(callback);
    }

    pub fn start(&self, thread_count: usize) {
        debug_assert!(self.threads.lock().unwrap().is_empty());
        for _ in 0..thread_count {
            let mut processor_loop = self.create_loop();

            self.threads.lock().unwrap().push(
                std::thread::Builder::new()
                    .name("Blck processing".to_string())
                    .spawn(move || {
                        processor_loop.run();
                    })
                    .unwrap(),
            );
        }
    }

    fn create_loop(&self) -> BlockProcessorLoop {
        BlockProcessorLoop {
            queue: self.process_queue.clone(),
            process: self.create_block_batch_processor(),
            throttler: self.throttler.clone(),
            clock: self.clock.clone(),
            last_throttle_log: None,
        }
    }

    fn create_block_batch_processor(&self) -> BlockBatchProcessor {
        BlockBatchProcessor {
            ledger: self.ledger.clone(),
            unchecked: self.unchecked.clone(),
            stats: self.process_stats.clone(),
            unchecked_reenqueuer: self.unchecked_reenqueuer.clone(),
            clock: self.clock.clone(),
        }
    }

    pub fn stop(&self) {
        self.process_queue.stop();
        let mut threads = self.threads.lock().unwrap();
        for join_handle in threads.drain(..) {
            join_handle.join().unwrap();
        }
    }
}

impl Drop for BlockProcessor {
    fn drop(&mut self) {
        self.stop();
    }
}

impl StatsSource for BlockProcessor {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.process_stats.collect_stats(result);
        self.throttler.collect_stats(result);
    }
}

struct BlockProcessorLoop {
    queue: Arc<BlockProcessorQueue>,
    process: BlockBatchProcessor,
    throttler: Arc<ProcessThrottler>,
    clock: Arc<SteadyClock>,
    last_throttle_log: Option<Timestamp>,
}

impl BlockProcessorLoop {
    fn run(&mut self) {
        while let Some(blocks) = self.queue.pop_blocking() {
            if self.queue.stopped() {
                break;
            }

            self.process(blocks);
        }
    }

    fn process(&mut self, blocks: VecDeque<Arc<BlockContext>>) {
        if let Some(duration) = self.throttler.throttle() {
            if self.should_log_throttle() {
                warn!("Throttling block processing!");
            }
            self.queue.wait(duration);
        }

        if !self.queue.stopped() {
            self.process.process_blocks(blocks);
        }
    }

    fn should_log_throttle(&mut self) -> bool {
        let now = self.clock.now();
        let should_log = match self.last_throttle_log {
            Some(i) => i.elapsed(now) >= Duration::from_secs(15),
            None => true,
        };
        if should_log {
            self.last_throttle_log = Some(now);
        }
        should_log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_test::traced_test;

    #[test]
    #[traced_test]
    fn throttle_logs_initially() {
        let mut processor_loop = BlockProcessorLoop {
            queue: Arc::new(BlockProcessorQueue::new_null()),
            process: BlockBatchProcessor::new_null(),
            throttler: ProcessThrottler::new(Arc::new(|| true)).into(),
            clock: SteadyClock::new_null().into(),
            last_throttle_log: None,
        };

        processor_loop.process(VecDeque::new());

        logs_assert(|logs| {
            if logs.len() != 1 {
                return Err(format!("len was {}, expected 1", logs.len()));
            }
            if !logs[0].contains("Throttling block processing!") {
                return Err(logs[0].to_owned());
            }
            Ok(())
        });
    }

    #[test]
    #[traced_test]
    fn throttle_suppresses_logs_for_15_secs() {
        let clock =
            SteadyClock::new_null_with_offsets([Duration::from_secs(14), Duration::from_secs(1)]);
        let mut processor_loop = BlockProcessorLoop {
            queue: Arc::new(BlockProcessorQueue::new_null()),
            process: BlockBatchProcessor::new_null(),
            throttler: ProcessThrottler::new(Arc::new(|| true)).into(),
            clock: clock.into(),
            last_throttle_log: None,
        };

        processor_loop.process(VecDeque::new());
        processor_loop.process(VecDeque::new());
        processor_loop.process(VecDeque::new());

        logs_assert(|logs| {
            if logs.len() != 2 {
                Err(format!("Expected 2 log entries, but found: {}", logs.len()))
            } else {
                Ok(())
            }
        });
    }
}
