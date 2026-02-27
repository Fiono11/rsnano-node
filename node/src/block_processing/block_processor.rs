use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

use rsnano_ledger::{Ledger, LedgerEvent};
use rsnano_nullable_clock::SteadyClock;
use rsnano_utils::{
    stats::{StatsCollection, StatsSource},
    sync::backpressure_channel::Sender,
};

use super::{
    BlockProcessorQueue, UncheckedBlockReenqueuer, UncheckedMap, backlog_waiter::BacklogWaiter,
    block_batch_processor::BlockBatchProcessorStats,
};
use crate::block_processing::block_batch_processor::BlockBatchProcessor;

pub struct BlockProcessor {
    threads: Mutex<Vec<JoinHandle<()>>>,
    process_queue: Arc<BlockProcessorQueue>,
    ledger: Arc<Ledger>,
    unchecked: Arc<Mutex<UncheckedMap>>,
    process_stats: Arc<BlockBatchProcessorStats>,
    backlog_waiter: Arc<BacklogWaiter>,
    event_publisher: Mutex<Option<Sender<LedgerEvent>>>,
    unchecked_reenqueuer: UncheckedBlockReenqueuer,
    clock: Arc<SteadyClock>,
}

impl BlockProcessor {
    pub(crate) fn new(
        process_queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        unchecked: Arc<Mutex<UncheckedMap>>,
        unchecked_reenqueuer: UncheckedBlockReenqueuer,
        backlog_waiter: Arc<BacklogWaiter>,
        event_publisher: Sender<LedgerEvent>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        Self {
            process_queue,
            ledger,
            unchecked,
            unchecked_reenqueuer,
            process_stats: Arc::new(BlockBatchProcessorStats::default()),
            threads: Mutex::new(Vec::new()),
            backlog_waiter,
            event_publisher: Mutex::new(Some(event_publisher)),
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
            backlog_waiter: self.backlog_waiter.clone(),
        }
    }

    fn create_block_batch_processor(&self) -> BlockBatchProcessor {
        BlockBatchProcessor {
            ledger: self.ledger.clone(),
            unchecked: self.unchecked.clone(),
            stats: self.process_stats.clone(),
            event_publisher: self
                .event_publisher
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .clone(),
            unchecked_reenqueuer: self.unchecked_reenqueuer.clone(),
            clock: self.clock.clone(),
        }
    }

    pub fn stop(&self) {
        drop(self.event_publisher.lock().unwrap().take());
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
    backlog_waiter: Arc<BacklogWaiter>,
}

impl BlockProcessorLoop {
    fn run(&mut self) {
        while let Some(blocks) = self.queue.pop_blocking() {
            self.backlog_waiter.wait_for_backlog();

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
        let backlog_waiter = Arc::new(BacklogWaiter::new_null());

        let mut processor_loop = BlockProcessorLoop {
            queue,
            process,
            backlog_waiter: backlog_waiter.clone(),
        };

        processor_loop.run();

        assert_eq!(backlog_waiter.call_count(), 1);
    }
}
