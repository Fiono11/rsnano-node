mod account_ack_processor;
mod block_ack_processor;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering::Relaxed},
};

use tracing::trace;

use rsnano_ledger::BlockSource;
use rsnano_messages::{AscPullAck, AscPullAckType};
use rsnano_network::ChannelId;
use rsnano_nullable_clock::Timestamp;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use super::query_tracker::QueryTracker;
use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::{
        bootstrap_queue::BootstrapQueue,
        frontier_scan::frontier_check_pool::FrontierCheckPool,
        query_tracker::{ProcessError, ProcessInfo, RunningQuery},
        response_processor::{
            account_ack_processor::AccountAckProcessor, block_ack_processor::BlockAckProcessor,
        },
    },
};

pub(crate) struct ResponseProcessor {
    query_tracker: Arc<QueryTracker>,
    frontier_check_pool: Arc<FrontierCheckPool>,
    block_proc_queue: Arc<BlockProcessorQueue>,
    bootstrap_queue: Arc<BootstrapQueue>,
    account_ack_processor: AccountAckProcessor,
    block_ack_processor: BlockAckProcessor,
    response_blocks: AtomicU64,
    response_account: AtomicU64,
    response_frontiers: AtomicU64,
}

impl ResponseProcessor {
    pub(crate) fn new(
        query_tracker: Arc<QueryTracker>,
        bootstrap_queue: Arc<BootstrapQueue>,
        block_queue: Arc<BlockProcessorQueue>,
        frontier_check_pool: Arc<FrontierCheckPool>,
    ) -> Self {
        let account_ack_processor = AccountAckProcessor::new(bootstrap_queue.clone());
        let block_ack_processor = BlockAckProcessor::new(bootstrap_queue.clone());

        Self {
            query_tracker,
            frontier_check_pool,
            block_proc_queue: block_queue,
            bootstrap_queue,
            account_ack_processor,
            block_ack_processor,
            response_blocks: AtomicU64::new(0),
            response_account: AtomicU64::new(0),
            response_frontiers: AtomicU64::new(0),
        }
    }

    pub fn process(
        &self,
        response: AscPullAck,
        channel_id: ChannelId,
        now: Timestamp,
    ) -> Result<ProcessInfo, ProcessError> {
        trace!(query_id = response.id, ?channel_id, "Process response");

        let query = self
            .query_tracker
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
                self.frontier_check_pool.process(query, frontiers)
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
