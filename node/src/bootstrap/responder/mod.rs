use std::{
    cmp::min,
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

#[cfg(feature = "rai_protocol")]
use rsnano_ledger::LedgerSet;
use rsnano_ledger::{AnySet, ConfirmedSet, Ledger, OwningAnySet};
use rsnano_messages::{
    AccountInfoAckPayload, AccountInfoReqPayload, AscPullAck, AscPullAckType, AscPullReq,
    AscPullReqType, BlocksAckPayload, BlocksReqPayload, FrontiersReqPayload, HashType, Message,
};
use rsnano_network::{Channel, ChannelEvent, ChannelId, TrafficType, token_bucket::TokenBucket};
use rsnano_types::{Block, BlockHash, Frontier};
#[cfg(feature = "rai_protocol")]
use rsnano_types::{
    RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES, RaiEpochCloseAck, RaiEpochCloseEntry,
    RaiEpochCloseEntryState, RaiEpochClosePage, RaiEpochCloseReq,
};
use rsnano_utils::{
    EventHandler,
    fair_queue::FairQueue,
    stats::{DetailType, Direction, StatType, Stats},
};

#[cfg(feature = "rai_protocol")]
use crate::consensus::{RaiCloseState, RaiClosedSlotState};
use crate::transport::MessageSender;

#[derive(Clone, Debug, PartialEq)]
pub struct BootstrapResponderConfig {
    pub max_queue: usize,
    pub threads: usize,
    pub batch_size: usize,
    pub limiter: usize,
}

impl Default for BootstrapResponderConfig {
    fn default() -> Self {
        Self {
            max_queue: 16,
            threads: 1,
            batch_size: 64,
            limiter: 500,
        }
    }
}

/**
 * Processes bootstrap requests (`asc_pull_req` messages) and replies with bootstrap responses (`asc_pull_ack`)
 */
pub struct BootstrapResponder {
    config: BootstrapResponderConfig,
    stats: Arc<Stats>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    server_impl: Arc<BootstrapResponderImpl>,
    running: AtomicBool,
}

impl BootstrapResponder {
    /** Maximum number of blocks to send in a single response, cannot be higher than capacity of a single `asc_pull_ack` message */
    pub const MAX_BLOCKS: u8 = BlocksAckPayload::MAX_BLOCKS;
    pub const MAX_FRONTIERS: usize = AscPullAck::MAX_FRONTIERS;

    pub(crate) fn new(
        config: BootstrapResponderConfig,
        stats: Arc<Stats>,
        ledger: Arc<Ledger>,
        message_sender: MessageSender,
    ) -> Self {
        let max_queue = config.max_queue;
        let server_impl = Arc::new(BootstrapResponderImpl {
            stats: Arc::clone(&stats),
            ledger,
            batch_size: config.batch_size,
            on_response: Arc::new(Mutex::new(None)),
            condition: Condvar::new(),
            stopped: AtomicBool::new(false),
            queue: Mutex::new(FairQueue::new(move |_| max_queue, |_| 1)),
            message_sender: Mutex::new(message_sender),
            limiter: Mutex::new(TokenBucket::with_burst_ratio(config.limiter, 3.0)),
            #[cfg(feature = "rai_protocol")]
            rai_close_state: Mutex::new(None),
            #[cfg(feature = "rai_protocol")]
            rai_close_recovery: Mutex::new(None),
        });

        Self {
            config,
            stats: Arc::clone(&stats),
            threads: Mutex::new(Vec::new()),
            server_impl,
            running: AtomicBool::new(false),
        }
    }

    pub fn new_null() -> Self {
        BootstrapResponder::new(
            BootstrapResponderConfig::default(),
            Stats::default().into(),
            Ledger::new_null().into(),
            MessageSender::new_null(),
        )
    }

    pub fn start(&self) {
        debug_assert!(self.threads.lock().unwrap().is_empty());

        let mut threads = self.threads.lock().unwrap();
        for _ in 0..self.config.threads {
            let server_impl = Arc::clone(&self.server_impl);
            threads.push(
                std::thread::Builder::new()
                    .name("Bootstrap serv".to_string())
                    .spawn(move || {
                        server_impl.run();
                    })
                    .unwrap(),
            );
        }

        self.running.store(true, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.server_impl.stopped.store(true, Ordering::SeqCst);
        self.server_impl.condition.notify_all();

        let mut threads = self.threads.lock().unwrap();
        for thread in threads.drain(..) {
            thread.join().unwrap();
        }

        self.running.store(false, Ordering::Relaxed);
    }

    pub fn set_response_callback(&self, cb: Box<dyn Fn(&AscPullAck, &Arc<Channel>) + Send + Sync>) {
        *self.server_impl.on_response.lock().unwrap() = Some(cb);
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn set_rai_close_state(&self, close_state: Arc<std::sync::RwLock<RaiCloseState>>) {
        *self.server_impl.rai_close_state.lock().unwrap() = Some(close_state);
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn set_rai_close_recovery_callback(
        &self,
        callback: Arc<dyn Fn(rsnano_types::RaiEpoch) + Send + Sync>,
    ) {
        *self.server_impl.rai_close_recovery.lock().unwrap() = Some(callback);
    }

    pub fn enqueue(&self, message: AscPullReq, channel: Arc<Channel>) -> bool {
        if !self.running.load(Ordering::Relaxed) {
            return false;
        }

        if !self.verify(&message) {
            self.stats
                .inc(StatType::BootstrapServer, DetailType::Invalid);
            return false;
        }

        let req_type = DetailType::from(&message.req_type);
        let added = {
            let mut guard = self.server_impl.queue.lock().unwrap();
            guard.push(channel.channel_id(), (message, channel.clone()))
        };

        if added {
            self.stats
                .inc(StatType::BootstrapServer, DetailType::Request);
            self.stats.inc(StatType::BootstrapServerRequest, req_type);

            self.server_impl.condition.notify_one();
        } else {
            self.stats
                .inc(StatType::BootstrapServer, DetailType::Overfill);
            self.stats.inc(StatType::BootstrapServerOverfill, req_type);
        }

        added
    }

    fn verify(&self, message: &AscPullReq) -> bool {
        match &message.req_type {
            AscPullReqType::Blocks(i) => i.count > 0 && i.count <= Self::MAX_BLOCKS,
            AscPullReqType::AccountInfo(i) => !i.target.is_zero(),
            AscPullReqType::Frontiers(i) => i.count > 0 && i.count as usize <= Self::MAX_FRONTIERS,
            #[cfg(feature = "rai_protocol")]
            AscPullReqType::RaiEpochClose(i) => {
                i.max_entries > 0 && i.max_entries <= RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES
            }
        }
    }
}

impl Drop for BootstrapResponder {
    fn drop(&mut self) {
        debug_assert!(self.threads.lock().unwrap().is_empty());
    }
}

impl EventHandler<ChannelEvent> for BootstrapResponder {
    fn handle(&self, event: &ChannelEvent) {
        self.server_impl.handle(event);
    }
}

struct BootstrapResponderImpl {
    stats: Arc<Stats>,
    ledger: Arc<Ledger>,
    on_response: Arc<Mutex<Option<Box<dyn Fn(&AscPullAck, &Arc<Channel>) + Send + Sync>>>>,
    stopped: AtomicBool,
    condition: Condvar,
    queue: Mutex<FairQueue<ChannelId, (AscPullReq, Arc<Channel>)>>,
    batch_size: usize,
    message_sender: Mutex<MessageSender>,
    limiter: Mutex<TokenBucket>,
    #[cfg(feature = "rai_protocol")]
    rai_close_state: Mutex<Option<Arc<std::sync::RwLock<RaiCloseState>>>>,
    #[cfg(feature = "rai_protocol")]
    rai_close_recovery: Mutex<Option<Arc<dyn Fn(rsnano_types::RaiEpoch) + Send + Sync>>>,
}

impl BootstrapResponderImpl {
    fn run(&self) {
        let mut queue = self.queue.lock().unwrap();
        while !self.stopped.load(Ordering::SeqCst) {
            queue = self
                .condition
                .wait_while(queue, |q| {
                    q.is_empty() && !self.stopped.load(Ordering::SeqCst)
                })
                .unwrap();

            // Rate limit the processing
            while !self.stopped.load(Ordering::SeqCst)
                && !self.limiter.lock().unwrap().try_consume(self.batch_size)
            {
                self.stats
                    .inc(StatType::BootstrapServer, DetailType::Cooldown);
                queue = self
                    .condition
                    .wait_timeout(queue, Duration::from_millis(100))
                    .unwrap()
                    .0;
            }

            if self.stopped.load(Ordering::SeqCst) {
                return;
            }

            if !queue.is_empty() {
                self.stats.inc(StatType::BootstrapServer, DetailType::Loop);
                queue = self.run_batch(queue);
            }
        }
    }

    fn run_batch<'a>(
        &'a self,
        mut queue: MutexGuard<'a, FairQueue<ChannelId, (AscPullReq, Arc<Channel>)>>,
    ) -> MutexGuard<'a, FairQueue<ChannelId, (AscPullReq, Arc<Channel>)>> {
        let batch = queue.next_batch(self.batch_size);
        drop(queue);

        let mut any = self.ledger.any();
        for (_, (request, channel)) in batch {
            if any.should_refresh() {
                any = self.ledger.any();
            }

            if !channel.should_drop(TrafficType::BootstrapServer) {
                let response = self.process(&any, request);
                self.respond(response, &channel);
            } else {
                self.stats.inc_dir(
                    StatType::BootstrapServer,
                    DetailType::ChannelFull,
                    Direction::Out,
                );
            }
        }

        self.queue.lock().unwrap()
    }

    fn process(&self, any: &OwningAnySet, message: AscPullReq) -> AscPullAck {
        match message.req_type {
            AscPullReqType::Blocks(blocks) => self.process_blocks(any, message.id, blocks),
            AscPullReqType::AccountInfo(account) => self.process_account(any, message.id, account),
            AscPullReqType::Frontiers(frontiers) => {
                self.process_frontiers(any, message.id, frontiers)
            }
            #[cfg(feature = "rai_protocol")]
            AscPullReqType::RaiEpochClose(request) => {
                self.process_rai_epoch_close(message.id, request)
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn process_rai_epoch_close(&self, id: u64, request: RaiEpochCloseReq) -> AscPullAck {
        let page = self.rai_epoch_close_page(request);
        AscPullAck {
            id,
            pull_type: AscPullAckType::RaiEpochClose(match page {
                Some(page) => RaiEpochCloseAck::new(page),
                None => RaiEpochCloseAck::unavailable(),
            }),
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_epoch_close_page(&self, request: RaiEpochCloseReq) -> Option<RaiEpochClosePage> {
        let close_state = self.rai_close_state.lock().unwrap().clone()?;
        let close_state = close_state.read().unwrap();
        let server_epoch = close_state.current_epoch();
        let close_hash = close_state.certified_close_hash(request.epoch)?;
        let cut = close_state.cut_set(request.epoch)?;
        if !close_state.cut_drained(request.epoch) {
            return None;
        }

        let mut entries = Vec::with_capacity(cut.len());
        for slot in cut {
            let state = close_state.closed_slot_state(request.epoch, slot)?;
            entries.push(RaiEpochCloseEntry {
                slot: *slot,
                state: epoch_close_entry_state_from_closed(*state),
            });
        }

        let start = request.start_index as usize;
        if start > entries.len() {
            return None;
        }
        let max = min(request.max_entries, RAI_EPOCH_CLOSE_PAGE_MAX_ENTRIES) as usize;
        let end = min(entries.len(), start.saturating_add(max));
        let page_entries = entries[start..end].to_vec();

        if request.epoch == server_epoch.saturating_sub(2)
            && let Some(callback) = self.rai_close_recovery.lock().unwrap().clone()
        {
            callback(request.epoch);
        }

        Some(
            RaiEpochClosePage::new(
                request.epoch,
                entries.len() as u32,
                request.start_index,
                close_hash,
                page_entries,
            )
            .with_server_epoch(server_epoch),
        )
    }

    fn process_blocks(&self, any: &dyn AnySet, id: u64, request: BlocksReqPayload) -> AscPullAck {
        let count = min(request.count, BootstrapResponder::MAX_BLOCKS);

        match request.start_type {
            HashType::Account => {
                let account = request.start.into();
                if let Some(info) = any.get_account(&account) {
                    // Start from open block if pulling by account
                    #[cfg(feature = "rai_protocol")]
                    {
                        return self.prepare_confirmed_response(
                            any,
                            id,
                            account,
                            info.open_block,
                            count,
                        );
                    }
                    #[cfg(not(feature = "rai_protocol"))]
                    return self.prepare_response(any, id, info.open_block, count);
                }
            }
            HashType::Block => {
                let start = request.start.into();
                if any.block_exists(&start) {
                    #[cfg(feature = "rai_protocol")]
                    {
                        if let Some(account) = any.block_account(&start) {
                            return self.prepare_confirmed_response(any, id, account, start, count);
                        }
                    }
                    #[cfg(not(feature = "rai_protocol"))]
                    return self.prepare_response(any, id, start, count);
                }
            }
        }

        // Neither block nor account found, send empty response to indicate that
        self.prepare_empty_blocks_response(id)
    }

    /*
     * Account info request
     */

    fn process_account(
        &self,
        any: &dyn AnySet,
        id: u64,
        request: AccountInfoReqPayload,
    ) -> AscPullAck {
        let target = match request.target_type {
            HashType::Account => request.target.into(),
            HashType::Block => {
                // Try to lookup account assuming target is block hash
                any.block_account(&request.target.into())
                    .unwrap_or_default()
            }
        };

        let mut response_payload = AccountInfoAckPayload {
            account: target,
            ..Default::default()
        };

        if let Some(account_info) = any.get_account(&target) {
            response_payload.account_open = account_info.open_block;
            response_payload.account_head = account_info.head;
            response_payload.account_block_count = account_info.block_count;

            #[cfg(feature = "rai_protocol")]
            if let Some(frontier) = self.rai_certified_frontier(&target) {
                // Dependency bootstrap uses account-info to decide whether a
                // source block is safe to request. A block finalized by a RAI
                // close record may legitimately be ahead of the legacy
                // cemented frontier, so expose the same certified frontier
                // used by block and frontier responses.
                if let Some(block) = any.get_block(&frontier) {
                    response_payload.account_conf_frontier = frontier;
                    response_payload.account_conf_height = block.height();
                }
            } else if let Some(conf_info) = any.confirmed().get_conf_info(&target) {
                response_payload.account_conf_frontier = conf_info.frontier;
                response_payload.account_conf_height = conf_info.height;
            }

            #[cfg(not(feature = "rai_protocol"))]
            if let Some(conf_info) = any.confirmed().get_conf_info(&target) {
                response_payload.account_conf_frontier = conf_info.frontier;
                response_payload.account_conf_height = conf_info.height;
            }
        }
        // If account is missing the response payload will contain all 0 fields, except for the target
        //
        AscPullAck {
            id,
            pull_type: AscPullAckType::AccountInfo(response_payload),
        }
    }

    /*
     * Frontiers request
     */
    fn process_frontiers(
        &self,
        any: &OwningAnySet,
        id: u64,
        request: FrontiersReqPayload,
    ) -> AscPullAck {
        #[cfg(feature = "rai_protocol")]
        let frontiers = any
            .accounts_range(request.start..)
            .filter_map(|(account, _)| {
                self.rai_certified_frontier(&account)
                    .or_else(|| {
                        any.confirmed()
                            .get_conf_info(&account)
                            .map(|info| info.frontier)
                    })
                    .map(|frontier| Frontier::new(account, frontier))
            })
            .take(request.count as usize)
            .collect();

        #[cfg(not(feature = "rai_protocol"))]
        let frontiers = any
            .accounts_range(request.start..)
            .map(|(account, info)| Frontier::new(account, info.head))
            .take(request.count as usize)
            .collect();

        AscPullAck {
            id,
            pull_type: AscPullAckType::Frontiers(frontiers),
        }
    }

    #[cfg(feature = "rai_protocol")]
    fn prepare_confirmed_response(
        &self,
        any: &dyn AnySet,
        id: u64,
        account: rsnano_types::Account,
        start_block: BlockHash,
        count: u8,
    ) -> AscPullAck {
        if let Some(frontier) = self.rai_certified_frontier(&account) {
            if !any.block_exists(&start_block) {
                return self.prepare_empty_blocks_response(id);
            }
            return self.prepare_response_until(any, id, start_block, count, Some(frontier));
        }

        let Some(conf_info) = any.confirmed().get_conf_info(&account) else {
            return self.prepare_empty_blocks_response(id);
        };

        if !any.confirmed().block_exists(&start_block) {
            return self.prepare_empty_blocks_response(id);
        }

        self.prepare_response_until(any, id, start_block, count, Some(conf_info.frontier))
    }

    #[cfg(feature = "rai_protocol")]
    fn rai_certified_frontier(&self, account: &rsnano_types::Account) -> Option<BlockHash> {
        let close_state = self.rai_close_state.lock().unwrap().clone()?;
        let close_state = close_state.read().unwrap();
        let current_epoch = close_state.current_epoch();

        (0..=current_epoch).rev().find_map(|epoch| {
            let close_hash = close_state.certified_close_hash(epoch)?;
            close_state
                .close_record_value(epoch, &close_hash)?
                .record
                .frontiers
                .get(account)
                .copied()
        })
    }

    #[cfg(not(feature = "rai_protocol"))]
    fn prepare_response(
        &self,
        any: &dyn AnySet,
        id: u64,
        start_block: BlockHash,
        count: u8,
    ) -> AscPullAck {
        self.prepare_response_until(any, id, start_block, count, None)
    }

    fn prepare_response_until(
        &self,
        any: &dyn AnySet,
        id: u64,
        start_block: BlockHash,
        count: u8,
        stop_block: Option<BlockHash>,
    ) -> AscPullAck {
        let blocks = self.prepare_blocks(any, start_block, count as usize, stop_block);
        let response_payload = BlocksAckPayload::new(blocks);

        AscPullAck {
            id,
            pull_type: AscPullAckType::Blocks(response_payload),
        }
    }

    fn prepare_empty_blocks_response(&self, id: u64) -> AscPullAck {
        AscPullAck {
            id,
            pull_type: AscPullAckType::Blocks(BlocksAckPayload::new(VecDeque::new())),
        }
    }

    fn prepare_blocks(
        &self,
        any: &dyn AnySet,
        start_block: BlockHash,
        count: usize,
        stop_block: Option<BlockHash>,
    ) -> VecDeque<Block> {
        let mut result = VecDeque::new();
        if !start_block.is_zero() {
            let mut current = any.get_block(&start_block);
            while let Some(c) = current.take() {
                let successor = any.block_successor(&c.hash()).unwrap_or_default();
                let reached_stop = Some(c.hash()) == stop_block;
                result.push_back(c.into());

                if result.len() == count || reached_stop {
                    break;
                }
                current = any.get_block(&successor);
            }
        }
        result
    }

    fn respond(&self, response: AscPullAck, channel: &Arc<Channel>) {
        self.stats.inc_dir(
            StatType::BootstrapServer,
            DetailType::Response,
            Direction::Out,
        );
        self.stats.inc(
            StatType::BootstrapServerResponse,
            DetailType::from(&response.pull_type),
        );

        // Increase relevant stats depending on payload type
        match &response.pull_type {
            AscPullAckType::Blocks(blocks) => {
                self.stats.add_dir(
                    StatType::BootstrapServer,
                    DetailType::Blocks,
                    Direction::Out,
                    blocks.blocks().len() as u64,
                );
            }
            AscPullAckType::AccountInfo(_) => {
                self.stats.inc_dir(
                    StatType::BootstrapServer,
                    DetailType::AccountInfo,
                    Direction::Out,
                );
            }
            AscPullAckType::Frontiers(frontiers) => {
                self.stats.add_dir(
                    StatType::BootstrapServer,
                    DetailType::Frontiers,
                    Direction::Out,
                    frontiers.len() as u64,
                );
            }
            #[cfg(feature = "rai_protocol")]
            AscPullAckType::RaiEpochClose(ack) => {
                self.stats.add_dir(
                    StatType::BootstrapServer,
                    DetailType::RaiEpochClose,
                    Direction::Out,
                    ack.page
                        .as_ref()
                        .map(|page| page.entries.len() as u64)
                        .unwrap_or_default(),
                );
            }
        }

        {
            let callback = self.on_response.lock().unwrap();
            if let Some(cb) = &*callback {
                (cb)(&response, channel);
            }
        }

        let msg = Message::AscPullAck(response);
        self.message_sender
            .lock()
            .unwrap()
            .try_send(channel, &msg, TrafficType::BootstrapServer);
    }
}

#[cfg(feature = "rai_protocol")]
fn epoch_close_entry_state_from_closed(state: RaiClosedSlotState) -> RaiEpochCloseEntryState {
    match state {
        RaiClosedSlotState::Finalized(block) => RaiEpochCloseEntryState::Finalized(block),
        RaiClosedSlotState::Carry(block) => RaiEpochCloseEntryState::Carry(block),
        RaiClosedSlotState::Released => RaiEpochCloseEntryState::Released,
    }
}

impl EventHandler<ChannelEvent> for BootstrapResponderImpl {
    fn handle(&self, event: &ChannelEvent) {
        if let ChannelEvent::Removed(id) = event {
            self.queue.lock().unwrap().remove(id);
        }
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use rsnano_ledger::{DEV_GENESIS_ACCOUNT, LedgerInserter};
    use rsnano_types::{Account, Amount, BlockHash, RaiEpochCloseReq, RaiSlot};

    #[test]
    fn rai_block_response_stops_at_confirmed_frontier() {
        let fixture = Fixture::new();
        let response = fixture.process_blocks(BlocksReqPayload {
            start_type: HashType::Account,
            start: (*DEV_GENESIS_ACCOUNT).into(),
            count: 10,
        });

        let AscPullAckType::Blocks(blocks) = response.pull_type else {
            panic!("expected blocks response");
        };
        let hashes = blocks
            .blocks()
            .iter()
            .map(|block| block.hash())
            .collect::<Vec<_>>();

        assert_eq!(
            hashes,
            vec![fixture.ledger.genesis().hash(), fixture.confirmed.hash()]
        );
    }

    #[test]
    fn rai_block_response_serves_through_certified_epoch_frontier() {
        let fixture = Fixture::new();
        let slot = RaiSlot::new(*DEV_GENESIS_ACCOUNT, 1);
        let mut close_state = RaiCloseState::new();
        close_state.start_closing(0).unwrap();
        close_state
            .install_cut(0, [slot].into_iter().collect())
            .unwrap();
        close_state
            .record_cut_drain(
                0,
                [(
                    slot,
                    RaiClosedSlotState::Finalized(fixture.unconfirmed.hash()),
                )],
            )
            .unwrap();
        let close_hash = close_state.record_current_close_record_value(0).unwrap();
        close_state.certify_close_record(0, &close_hash).unwrap();
        fixture
            .responder
            .set_rai_close_state(Arc::new(std::sync::RwLock::new(close_state)));

        let response = fixture.process_blocks(BlocksReqPayload {
            start_type: HashType::Account,
            start: (*DEV_GENESIS_ACCOUNT).into(),
            count: 10,
        });

        let AscPullAckType::Blocks(blocks) = response.pull_type else {
            panic!("expected blocks response");
        };
        assert_eq!(
            blocks
                .blocks()
                .iter()
                .map(|block| block.hash())
                .collect::<Vec<_>>(),
            vec![
                fixture.ledger.genesis().hash(),
                fixture.confirmed.hash(),
                fixture.unconfirmed.hash(),
            ]
        );
    }

    #[test]
    fn rai_frontier_response_reports_confirmed_frontier() {
        let fixture = Fixture::new();
        let any = fixture.ledger.any();
        let response = fixture.responder.server_impl.process(
            &any,
            AscPullReq {
                id: 7,
                req_type: AscPullReqType::Frontiers(FrontiersReqPayload {
                    start: *DEV_GENESIS_ACCOUNT,
                    count: 10,
                }),
            },
        );

        let AscPullAckType::Frontiers(frontiers) = response.pull_type else {
            panic!("expected frontiers response");
        };

        assert_eq!(frontiers[0].account, *DEV_GENESIS_ACCOUNT);
        assert_eq!(frontiers[0].hash, fixture.confirmed.hash());
    }

    #[test]
    fn rai_frontier_response_reports_certified_epoch_frontier() {
        let fixture = Fixture::new();
        fixture.install_certified_unconfirmed_frontier();
        let any = fixture.ledger.any();
        let response = fixture.responder.server_impl.process(
            &any,
            AscPullReq {
                id: 7,
                req_type: AscPullReqType::Frontiers(FrontiersReqPayload {
                    start: *DEV_GENESIS_ACCOUNT,
                    count: 10,
                }),
            },
        );

        let AscPullAckType::Frontiers(frontiers) = response.pull_type else {
            panic!("expected frontiers response");
        };
        assert_eq!(frontiers[0].account, *DEV_GENESIS_ACCOUNT);
        assert_eq!(frontiers[0].hash, fixture.unconfirmed.hash());
    }

    #[test]
    fn rai_account_info_reports_certified_epoch_frontier() {
        let fixture = Fixture::new();
        fixture.install_certified_unconfirmed_frontier();
        let any = fixture.ledger.any();
        let response = fixture.responder.server_impl.process(
            &any,
            AscPullReq {
                id: 7,
                req_type: AscPullReqType::AccountInfo(AccountInfoReqPayload {
                    target: (*DEV_GENESIS_ACCOUNT).into(),
                    target_type: HashType::Account,
                }),
            },
        );

        let AscPullAckType::AccountInfo(info) = response.pull_type else {
            panic!("expected account-info response");
        };
        assert_eq!(info.account_conf_frontier, fixture.unconfirmed.hash());
        assert_eq!(info.account_conf_height, fixture.unconfirmed.height());
    }

    #[test]
    fn rai_epoch_close_response_returns_drained_close_state_page() {
        let fixture = Fixture::new();
        let slot = RaiSlot::new(Account::from(42), 7);
        let block = BlockHash::from(9);
        let close_hash = {
            let mut close_state = RaiCloseState::new();
            close_state.start_closing(0).unwrap();
            close_state
                .install_cut(0, [slot].into_iter().collect())
                .unwrap();
            close_state
                .record_cut_drain(0, [(slot, RaiClosedSlotState::Finalized(block))])
                .unwrap();
            let close_hash = close_state.record_current_close_record_value(0).unwrap();
            close_state.certify_close_record(0, &close_hash).unwrap();
            fixture
                .responder
                .set_rai_close_state(Arc::new(std::sync::RwLock::new(close_state)));
            close_hash
        };

        let any = fixture.ledger.any();
        let response = fixture.responder.server_impl.process(
            &any,
            AscPullReq {
                id: 7,
                req_type: AscPullReqType::RaiEpochClose(RaiEpochCloseReq::new(0, 0)),
            },
        );

        let AscPullAckType::RaiEpochClose(ack) = response.pull_type else {
            panic!("expected RAI epoch close response");
        };
        let page = ack.page.expect("close state should be available");
        assert_eq!(page.epoch, 0);
        assert_eq!(page.total_entries, 1);
        assert_eq!(page.close_hash, close_hash);
        assert_eq!(page.entries[0].slot, slot);
        assert_eq!(
            page.entries[0].state,
            RaiEpochCloseEntryState::Finalized(block)
        );
    }

    struct Fixture {
        ledger: Arc<Ledger>,
        responder: BootstrapResponder,
        confirmed: rsnano_types::SavedBlock,
        unconfirmed: rsnano_types::SavedBlock,
    }

    impl Fixture {
        fn new() -> Self {
            let ledger = Arc::new(Ledger::new_null());
            let inserter = LedgerInserter::new(&ledger);
            let mut genesis = inserter.genesis();
            let confirmed = genesis.send(Account::from(1), Amount::raw(1));
            let unconfirmed = genesis.send(Account::from(2), Amount::raw(1));
            ledger.confirm(confirmed.hash());
            let responder = BootstrapResponder::new(
                BootstrapResponderConfig::default(),
                Stats::default().into(),
                ledger.clone(),
                MessageSender::new_null(),
            );

            Self {
                ledger,
                responder,
                confirmed,
                unconfirmed,
            }
        }

        fn process_blocks(&self, payload: BlocksReqPayload) -> AscPullAck {
            let any = self.ledger.any();
            self.responder.server_impl.process(
                &any,
                AscPullReq {
                    id: 7,
                    req_type: AscPullReqType::Blocks(payload),
                },
            )
        }

        fn install_certified_unconfirmed_frontier(&self) {
            let slot = RaiSlot::new(*DEV_GENESIS_ACCOUNT, 1);
            let mut close_state = RaiCloseState::new();
            close_state.start_closing(0).unwrap();
            close_state
                .install_cut(0, [slot].into_iter().collect())
                .unwrap();
            close_state
                .record_cut_drain(
                    0,
                    [(slot, RaiClosedSlotState::Finalized(self.unconfirmed.hash()))],
                )
                .unwrap();
            let close_hash = close_state.record_current_close_record_value(0).unwrap();
            close_state.certify_close_record(0, &close_hash).unwrap();
            self.responder
                .set_rai_close_state(Arc::new(std::sync::RwLock::new(close_state)));
        }
    }
}
