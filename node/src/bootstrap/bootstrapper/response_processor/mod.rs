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

use super::logic::BootstrapLogic;
use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::{
        logic::{ProcessError, ProcessInfo, RunningQuery},
        response_processor::frontier_check_pool::FrontierCheckPool,
    },
};

pub(crate) struct ResponseProcessor {
    logic: Arc<Mutex<BootstrapLogic>>,
    frontier_check_pool: FrontierCheckPool,
    block_proc_queue: Arc<BlockProcessorQueue>,
    response_blocks: AtomicU64,
    response_account: AtomicU64,
    response_frontiers: AtomicU64,
}

impl ResponseProcessor {
    pub(crate) fn new(
        logic: Arc<Mutex<BootstrapLogic>>,
        stats: Arc<Stats>,
        block_queue: Arc<BlockProcessorQueue>,
        ledger: Arc<Ledger>,
    ) -> Self {
        let frontier_check_pool = FrontierCheckPool::new(stats.clone(), ledger, logic.clone());

        Self {
            logic,
            frontier_check_pool,
            block_proc_queue: block_queue,
            response_blocks: AtomicU64::new(0),
            response_account: AtomicU64::new(0),
            response_frontiers: AtomicU64::new(0),
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

        let mut logic = self.logic.lock().unwrap();
        let query = logic.take_running_query_for(&response, channel_id)?;
        let process_info = self
            .process_response_for_query(&query, response, &mut logic)
            .map(|_| ProcessInfo::new(&query, now))?;

        self.enqueue_next_blocks(&mut logic);
        self.frontier_check_pool.enqueue_frontiers(&mut logic);
        Ok(process_info)
    }

    fn process_response_for_query(
        &self,
        query: &RunningQuery,
        response: AscPullAck,
        logic: &mut BootstrapLogic,
    ) -> Result<(), ProcessError> {
        let ok = match response.pull_type {
            AscPullAckType::Blocks(blocks) => {
                self.response_blocks.fetch_add(1, Relaxed);
                logic
                    .block_ack_processor
                    .process(&mut logic.bootstrap_queue, query, blocks)
            }
            AscPullAckType::AccountInfo(info) => {
                self.response_account.fetch_add(1, Relaxed);
                let acc_proc = &mut logic.account_ack_processor;
                let boot_queue = &logic.bootstrap_queue;
                acc_proc.process(boot_queue, query, &info)
            }
            AscPullAckType::Frontiers(frontiers) => {
                self.response_frontiers.fetch_add(1, Relaxed);
                logic.frontiers_processor.process(query, frontiers)
            }
        };

        if ok {
            Ok(())
        } else {
            Err(ProcessError::InvalidResponse)
        }
    }

    // TODO Remeove duplication! Copied from BlockInspector
    fn enqueue_next_blocks(&self, logic: &mut BootstrapLogic) {
        while let Some(block) = logic.bootstrap_queue.next_block_to_process() {
            let block_hash = block.hash();

            let inserted = self.block_proc_queue.push(BlockContext::new(
                block.clone(),
                BlockSource::Bootstrap,
                // TODO use real channel id
                ChannelId::LOOPBACK,
            ));

            if inserted {
                logic.bootstrap_queue.processing_started(&block_hash);
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
    }
}
