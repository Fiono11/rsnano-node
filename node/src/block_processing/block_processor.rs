use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

use rsnano_ledger::Ledger;
use rsnano_nullable_clock::SteadyClock;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use super::{
    BlockProcessorQueue, UncheckedBlockReenqueuer, UncheckedMap,
    block_batch_processor::BlockBatchProcessorStats,
};
use crate::block_processing::{
    block_batch_processor::BlockBatchProcessor, process_throttler::ProcessThrottler,
};

pub struct BlockProcessor {
    threads: Mutex<Vec<JoinHandle<()>>>,
    process_queue: Arc<BlockProcessorQueue>,
    ledger: Arc<Ledger>,
    unchecked: Arc<Mutex<UncheckedMap>>,
    process_stats: Arc<BlockBatchProcessorStats>,
    should_throttle: Arc<dyn Fn() -> bool + Send + Sync>,
    unchecked_reenqueuer: UncheckedBlockReenqueuer,
    clock: Arc<SteadyClock>,
}

impl BlockProcessor {
    pub(crate) fn new(
        process_queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        unchecked: Arc<Mutex<UncheckedMap>>,
        unchecked_reenqueuer: UncheckedBlockReenqueuer,
        should_throttle: Arc<dyn Fn() -> bool + Send + Sync>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            process_queue,
            ledger,
            unchecked,
            unchecked_reenqueuer,
            process_stats: Arc::new(BlockBatchProcessorStats::default()),
            threads: Mutex::new(Vec::new()),
            should_throttle,
            clock,
        }
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
            throttler: ProcessThrottler::new(
                self.process_queue.clone(),
                self.clock.clone(),
                self.should_throttle.clone(),
            ),
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
    }
}

struct BlockProcessorLoop {
    queue: Arc<BlockProcessorQueue>,
    process: BlockBatchProcessor,
    throttler: ProcessThrottler,
}

impl BlockProcessorLoop {
    fn run(&mut self) {
        while let Some(blocks) = self.queue.pop_blocking() {
            self.throttler.throttle();

            if self.queue.stopped() {
                break;
            }

            self.process.process_blocks(blocks);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_processing::BlockContext;

    #[test]
    fn wait_for_backlog() {
        let queue = Arc::new(BlockProcessorQueue::new_null_with(vec![
            BlockContext::new_test_instance().into(),
        ]));
        let process = BlockBatchProcessor::new_null();
        let throttler = ProcessThrottler::new_null();

        let mut processor_loop = BlockProcessorLoop {
            queue,
            process,
            throttler,
        };

        processor_loop.run();

        assert_eq!(processor_loop.throttler.call_count(), 1);
    }
}
