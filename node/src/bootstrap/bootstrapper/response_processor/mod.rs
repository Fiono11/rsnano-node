mod account_ack_processor;
mod block_ack_processor;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering::Relaxed},
};

use tracing::trace;

use rsnano_ledger::BlockSource;
use rsnano_messages::{AscPullAck, AscPullAckType};
#[cfg(feature = "rai_protocol")]
use rsnano_messages::{ConfirmReq, Message};
use rsnano_network::ChannelId;
use rsnano_nullable_clock::Timestamp;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use super::query_tracker::QueryTracker;
use crate::{
    block_processing::{BlockContext, BlockProcessorQueue},
    bootstrap::bootstrapper::{
        bootstrap_queue::BootstrapQueue,
        frontier_scan::FrontierScan,
        peer_scoring::PeerScoring,
        query_tracker::{ProcessError, ProcessInfo, QueryType, RunningQuery},
        response_processor::{
            account_ack_processor::AccountAckProcessor, block_ack_processor::BlockAckProcessor,
        },
    },
};
#[cfg(feature = "rai_protocol")]
use crate::{consensus::election::ConfirmedElection, transport::MessageSender};
#[cfg(feature = "rai_protocol")]
use bounded_vec_deque::BoundedVecDeque;
#[cfg(feature = "rai_protocol")]
use rsnano_ledger::{AnySet, Ledger};
#[cfg(feature = "rai_protocol")]
use rsnano_network::{Network, TrafficType};
#[cfg(feature = "rai_protocol")]
use std::sync::{Mutex, RwLock};

pub(crate) struct ResponseProcessor {
    query_tracker: Arc<QueryTracker>,
    peer_scoring: Arc<PeerScoring>,
    frontier_scan: Arc<FrontierScan>,
    block_proc_queue: Arc<BlockProcessorQueue>,
    bootstrap_queue: Arc<BootstrapQueue>,
    account_ack_processor: AccountAckProcessor,
    block_ack_processor: BlockAckProcessor,
    response_blocks: AtomicU64,
    response_account: AtomicU64,
    response_frontiers: AtomicU64,
    #[cfg(feature = "rai_protocol")]
    ledger: Arc<Ledger>,
    #[cfg(feature = "rai_protocol")]
    network: Arc<RwLock<Network>>,
    #[cfg(feature = "rai_protocol")]
    message_sender: Mutex<MessageSender>,
    #[cfg(feature = "rai_protocol")]
    recently_cemented: Arc<Mutex<BoundedVecDeque<ConfirmedElection>>>,
}

impl ResponseProcessor {
    pub(crate) fn new(
        query_tracker: Arc<QueryTracker>,
        peer_scoring: Arc<PeerScoring>,
        bootstrap_queue: Arc<BootstrapQueue>,
        block_queue: Arc<BlockProcessorQueue>,
        frontier_scan: Arc<FrontierScan>,
        #[cfg(feature = "rai_protocol")] ledger: Arc<Ledger>,
        #[cfg(feature = "rai_protocol")] network: Arc<RwLock<Network>>,
        #[cfg(feature = "rai_protocol")] message_sender: MessageSender,
        #[cfg(feature = "rai_protocol")] recently_cemented: Arc<
            Mutex<BoundedVecDeque<ConfirmedElection>>,
        >,
    ) -> Self {
        let account_ack_processor = AccountAckProcessor::new(bootstrap_queue.clone());
        let block_ack_processor =
            BlockAckProcessor::new(bootstrap_queue.clone(), peer_scoring.clone());

        Self {
            query_tracker,
            peer_scoring,
            frontier_scan,
            block_proc_queue: block_queue,
            bootstrap_queue,
            account_ack_processor,
            block_ack_processor,
            response_blocks: AtomicU64::new(0),
            response_account: AtomicU64::new(0),
            response_frontiers: AtomicU64::new(0),
            #[cfg(feature = "rai_protocol")]
            ledger,
            #[cfg(feature = "rai_protocol")]
            network,
            #[cfg(feature = "rai_protocol")]
            message_sender: Mutex::new(message_sender),
            #[cfg(feature = "rai_protocol")]
            recently_cemented,
        }
    }

    pub fn process(
        &self,
        response: AscPullAck,
        now: Timestamp,
    ) -> Result<ProcessInfo, ProcessError> {
        trace!(query_id = response.id, "Process response");

        let query = self.query_tracker.take_running_query_for(&response)?;

        if !query.is_valid_response_type(&response) {
            // The running query was consumed and is gone from the tracker, so
            // it can never time out. Release the peer's in-flight slot and the
            // query's bootstrap queue state now — skipping this would leak
            // them forever.
            self.peer_scoring.query_completed(query.channel_id);
            self.requeue_query_target(&query);
            return Err(ProcessError::InvalidResponseType);
        }

        self.peer_scoring.response_received(query.channel_id);

        let process_info = self
            .process_response_for_query(&query, response)
            .map(|_| ProcessInfo::new(&query, now))?;

        self.enqueue_next_blocks();
        self.frontier_scan.enqueue_frontiers();
        Ok(process_info)
    }

    /// Removes all timed out queries from the query tracker and releases
    /// the state they hold in the bootstrap queue, so that their targets
    /// can be requested again
    pub fn process_timeouts(&self) {
        for query in self.query_tracker.timeout() {
            trace!(
                query_id = query.id,
                query_type = ?query.query_type,
                "Query timed out"
            );
            self.peer_scoring.timed_out(query.channel_id);
            self.requeue_query_target(&query);
        }
    }

    /// A query that yielded no usable response still holds state in the
    /// bootstrap queue (a downloading account or a requested dependency).
    /// Release that state, so that the target can be requested again.
    fn requeue_query_target(&self, query: &RunningQuery) {
        match query.query_type {
            QueryType::BlocksByHash | QueryType::BlocksByAccount => {
                self.bootstrap_queue.requeue_download(&query.account);
            }
            QueryType::AccountInfoByHash => {
                self.bootstrap_queue.remove_dependency_request(&query.hash);
            }
            // Frontier heads recover via their own cooldown
            QueryType::Frontiers | QueryType::Invalid => {}
        }
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
                #[cfg(feature = "rai_protocol")]
                self.request_earlier_epoch_votes(query, &frontiers);
                self.frontier_scan.process(query, frontiers)
            }
        };

        if ok {
            Ok(())
        } else {
            Err(ProcessError::InvalidResponse)
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn request_earlier_epoch_votes(
        &self,
        query: &RunningQuery,
        frontiers: &[rsnano_types::Frontier],
    ) {
        let local_epochs = {
            let history = self.recently_cemented.lock().unwrap();
            let mut epochs = std::collections::HashMap::with_capacity(history.len());
            for entry in history.iter().filter(|entry| entry.epoch > 0) {
                epochs
                    .entry(entry.winner.hash())
                    .and_modify(|epoch: &mut u64| *epoch = (*epoch).min(entry.epoch))
                    .or_insert(entry.epoch);
            }
            epochs
        };
        let mut by_epoch = std::collections::HashMap::<u64, Vec<_>>::new();
        let any = self.ledger.any();
        for frontier in frontiers.iter().filter(|frontier| frontier.epoch > 0) {
            let local = local_epochs.get(&frontier.hash).copied();
            if local.is_none_or(|epoch| frontier.epoch < epoch)
                && let Some(block) = any.get_block(&frontier.hash)
            {
                by_epoch
                    .entry(frontier.epoch)
                    .or_default()
                    .push((frontier.hash, block.root()));
            }
        }
        let channel = self.network.read().unwrap().get(query.channel_id).cloned();
        let Some(channel) = channel else {
            return;
        };
        for (epoch, requests) in by_epoch {
            for chunk in requests.chunks(ConfirmReq::HASHES_MAX) {
                let message =
                    Message::ConfirmReq(ConfirmReq::new(chunk.to_vec()).with_epoch(epoch));
                self.message_sender.lock().unwrap().try_send(
                    &channel,
                    &message,
                    TrafficType::ConfirmationRequests,
                );
            }
        }
    }

    // TODO Remeove duplication! Copied from BlockInspector
    fn enqueue_next_blocks(&self) {
        while let Some(block) = self.bootstrap_queue.take_next_block_for_processing() {
            let block_hash = block.hash();

            let inserted = self.block_proc_queue.push(BlockContext::new(
                block,
                BlockSource::Bootstrap,
                // TODO use real channel id
                ChannelId::LOOPBACK,
            ));

            if !inserted {
                // block processor queue is full — undo the hand-off so the block
                // stays in ready_to_process and can be retried later.
                self.bootstrap_queue.revert_processing_started(&block_hash);
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

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{Account, Block, BlockHash, PrivateKey, StateBlockArgs};
    use std::time::Duration;

    #[test]
    fn timed_out_blocks_query_requeues_account() {
        let fixture = create_fixture();
        let account = Account::from(1);
        fixture.queue.insert(account);
        fixture.queue.download_started(&account);
        fixture.tracker.insert(RunningQuery {
            query_type: QueryType::BlocksByAccount,
            account,
            response_cutoff: Timestamp::new_test_instance() - Duration::from_secs(1),
            ..RunningQuery::new_test_instance()
        });

        fixture.processor.process_timeouts();

        assert_eq!(fixture.tracker.query_count(), 0);
        assert_eq!(fixture.queue.info().downloading, 0);
        let target = fixture.queue.next_download_target().unwrap();
        assert_eq!(target.account, account);
    }

    #[test]
    fn query_that_is_not_timed_out_is_kept() {
        let fixture = create_fixture();
        let account = Account::from(1);
        fixture.queue.insert(account);
        fixture.queue.download_started(&account);
        fixture.tracker.insert(RunningQuery {
            query_type: QueryType::BlocksByAccount,
            account,
            response_cutoff: Timestamp::new_test_instance() + Duration::from_secs(1),
            ..RunningQuery::new_test_instance()
        });

        fixture.processor.process_timeouts();

        assert_eq!(fixture.tracker.query_count(), 1);
        assert_eq!(fixture.queue.info().downloading, 1);
    }

    #[test]
    fn timed_out_dependency_query_makes_dependency_requestable_again() {
        let fixture = create_fixture();
        let key = PrivateKey::from(42);
        let dependency = BlockHash::from(100);
        make_blocked_account(&fixture.queue, &key, dependency);
        fixture.queue.dependency_account_requested(&dependency);
        assert_eq!(fixture.queue.next_unknown_blocking_hash(), None);
        fixture.tracker.insert(RunningQuery {
            query_type: QueryType::AccountInfoByHash,
            hash: dependency,
            response_cutoff: Timestamp::new_test_instance() - Duration::from_secs(1),
            ..RunningQuery::new_test_instance()
        });

        fixture.processor.process_timeouts();

        assert_eq!(fixture.tracker.query_count(), 0);
        assert_eq!(fixture.queue.next_unknown_blocking_hash(), Some(dependency));
    }

    #[test]
    fn timed_out_frontier_query_is_dropped() {
        let fixture = create_fixture();
        fixture.tracker.insert(RunningQuery {
            query_type: QueryType::Frontiers,
            account: Account::ZERO,
            hash: BlockHash::ZERO,
            response_cutoff: Timestamp::new_test_instance() - Duration::from_secs(1),
            ..RunningQuery::new_test_instance()
        });

        fixture.processor.process_timeouts();

        assert_eq!(fixture.tracker.query_count(), 0);
    }

    #[test]
    fn invalid_response_type_requeues_account() {
        let fixture = create_fixture();
        let account = Account::from(1);
        let query_id = 7;
        fixture.queue.insert(account);
        fixture.queue.download_started(&account);
        fixture.tracker.insert(RunningQuery {
            id: query_id,
            query_type: QueryType::BlocksByAccount,
            account,
            ..RunningQuery::new_test_instance()
        });
        let response = AscPullAck {
            id: query_id,
            pull_type: AscPullAckType::Frontiers(Vec::new()),
        };

        let result = fixture
            .processor
            .process(response, Timestamp::new_test_instance());

        assert!(matches!(result, Err(ProcessError::InvalidResponseType)));
        assert_eq!(fixture.queue.info().downloading, 0);
        let target = fixture.queue.next_download_target().unwrap();
        assert_eq!(target.account, account);
    }

    /* Test helpers */

    fn create_fixture() -> Fixture {
        let tracker = Arc::new(QueryTracker::new_null());
        let queue = Arc::new(BootstrapQueue::new_null());
        let processor = ResponseProcessor::new(
            tracker.clone(),
            Arc::new(PeerScoring::default()),
            queue.clone(),
            Arc::new(BlockProcessorQueue::default()),
            Arc::new(FrontierScan::new_null()),
            #[cfg(feature = "rai_protocol")]
            Arc::new(Ledger::new_null()),
            #[cfg(feature = "rai_protocol")]
            Arc::new(RwLock::new(Network::new_null())),
            #[cfg(feature = "rai_protocol")]
            MessageSender::new_null(),
            #[cfg(feature = "rai_protocol")]
            Arc::new(Mutex::new(BoundedVecDeque::new(2048))),
        );
        Fixture {
            processor,
            tracker,
            queue,
        }
    }

    fn make_blocked_account(queue: &BootstrapQueue, key: &PrivateKey, dependency: BlockHash) {
        let account = key.account();
        let receive: Block = StateBlockArgs {
            key,
            link: dependency.into(),
            ..StateBlockArgs::new_test_instance()
        }
        .into();
        queue.insert(account);
        queue.download_started(&account);
        queue.download_finished(&account, [receive].into(), false, ChannelId::from(1));
        let next = queue.take_next_block_for_processing().unwrap();
        queue.block(&next.hash(), dependency);
    }

    struct Fixture {
        processor: ResponseProcessor,
        tracker: Arc<QueryTracker>,
        queue: Arc<BootstrapQueue>,
    }
}
