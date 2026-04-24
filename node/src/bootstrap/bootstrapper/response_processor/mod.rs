mod account_ack_processor;
mod block_ack_processor;
mod database_crawler;
mod frontier_check_pool;
mod frontier_checker;
mod frontier_worker;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering::Relaxed},
};
use tracing::trace;

use rsnano_ledger::{BlockSource, Ledger};
use rsnano_messages::{AscPullAck, AscPullAckType};
use rsnano_network::ChannelId;
use rsnano_nullable_clock::Timestamp;
use rsnano_utils::stats::{Stats, StatsCollection, StatsSource};

use super::query_tracker::QueryTracker;
use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::{
        VerifyResult,
        bootstrap_queue::BootstrapQueue,
        frontier_scan::{frontiers_processor::FrontiersProcessor, stats::FrontierScanStats},
        query_tracker::{ProcessError, ProcessInfo, RunningQuery},
        response_processor::{
            account_ack_processor::AccountAckProcessor, block_ack_processor::BlockAckProcessor,
            frontier_check_pool::FrontierCheckPool,
        },
    },
};

pub(crate) struct ResponseProcessor {
    logic: Arc<Mutex<QueryTracker>>,
    frontier_check_pool: FrontierCheckPool,
    block_proc_queue: Arc<BlockProcessorQueue>,
    bootstrap_queue: Arc<BootstrapQueue>,
    account_ack_processor: AccountAckProcessor,
    block_ack_processor: BlockAckProcessor,
    response_blocks: AtomicU64,
    response_account: AtomicU64,
    response_frontiers: AtomicU64,
    frontier_stats: Arc<FrontierScanStats>,
    frontiers_processor: Arc<FrontiersProcessor>,
}

impl ResponseProcessor {
    pub(crate) fn new(
        logic: Arc<Mutex<QueryTracker>>,
        bootstrap_queue: Arc<BootstrapQueue>,
        stats: Arc<Stats>,
        block_queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
        frontier_stats: Arc<FrontierScanStats>,
        frontiers_processor: Arc<FrontiersProcessor>,
    ) -> Self {
        let frontier_check_pool = FrontierCheckPool::new(
            stats.clone(),
            frontier_stats.clone(),
            ledger,
            bootstrap_queue.clone(),
            frontiers_processor.clone(),
        );

        let account_ack_processor = AccountAckProcessor::new(bootstrap_queue.clone());
        let block_ack_processor = BlockAckProcessor::new(bootstrap_queue.clone());

        Self {
            logic,
            frontier_check_pool,
            block_proc_queue: block_queue,
            bootstrap_queue,
            account_ack_processor,
            block_ack_processor,
            response_blocks: AtomicU64::new(0),
            response_account: AtomicU64::new(0),
            response_frontiers: AtomicU64::new(0),
            frontier_stats,
            frontiers_processor,
        }
    }

    pub fn set_max_pending_frontiers(&mut self, max_pending: usize) {
        self.frontier_check_pool.max_pending = max_pending;
    }

    pub fn process(
        &self,
        response: AscPullAck,
        channel_id: ChannelId,
        now: Timestamp,
    ) -> Result<ProcessInfo, ProcessError> {
        trace!(query_id = response.id, ?channel_id, "Process response");

        let query = self
            .logic
            .lock()
            .unwrap()
            .take_running_query_for(&response, channel_id)?;

        let process_info = self
            .process_response_for_query(&query, response)
            .map(|_| ProcessInfo::new(&query, now))?;

        self.enqueue_next_blocks();
        self.frontier_check_pool.enqueue_frontiers();
        Ok(process_info)
    }

    fn process_response_for_query(
        &self,
        query: &RunningQuery,
        response: AscPullAck,
    ) -> Result<(), ProcessError> {
        let ok = match response.pull_type {
            AscPullAckType::Blocks(blocks) => {
                self.response_blocks.fetch_add(1, Relaxed);
                self.block_ack_processor.process(query, blocks)
            }
            AscPullAckType::AccountInfo(info) => {
                self.response_account.fetch_add(1, Relaxed);
                self.account_ack_processor.process(query, &info)
            }
            AscPullAckType::Frontiers(frontiers) => {
                self.response_frontiers.fetch_add(1, Relaxed);
                match self.frontiers_processor.process(query, frontiers) {
                    VerifyResult::Ok => {
                        self.frontier_stats.verified.fetch_add(1, Relaxed);
                        true
                    }
                    VerifyResult::NothingNew => {
                        self.frontier_stats.nothing_new.fetch_add(1, Relaxed);
                        true
                    }
                    VerifyResult::Invalid => {
                        self.frontier_stats.invalid.fetch_add(1, Relaxed);
                        false
                    }
                }
            }
        };

        if ok {
            Ok(())
        } else {
            Err(ProcessError::InvalidResponse)
        }
    }

    // TODO Remeove duplication! Copied from BlockInspector
    fn enqueue_next_blocks(&self) {
        while let Some(block) = self.bootstrap_queue.next_block_to_process() {
            let block_hash = block.hash();

            let inserted = self.block_proc_queue.push(BlockContext::new(
                block.clone(),
                BlockSource::Bootstrap,
                // TODO use real channel id
                ChannelId::LOOPBACK,
            ));

            if inserted {
                self.bootstrap_queue.processing_started(&block_hash);
            } else {
                // block processor queue is full!
                break;
            }
        }
    }
}

impl StatsSource for ResponseProcessor {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const BOOTSTRAP_PROCESS: &str = "bootstrap_process";
        result.insert(
            BOOTSTRAP_PROCESS,
            "blocks",
            self.response_blocks.load(Relaxed),
        );
        result.insert(
            BOOTSTRAP_PROCESS,
            "account_info",
            self.response_account.load(Relaxed),
        );
        result.insert(
            BOOTSTRAP_PROCESS,
            "frontiers",
            self.response_frontiers.load(Relaxed),
        );
        self.account_ack_processor.collect_stats(result);
        self.block_ack_processor.collect_stats(result);
    }
}
