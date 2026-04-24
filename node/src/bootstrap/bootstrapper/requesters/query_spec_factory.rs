use std::sync::{atomic::Ordering::Relaxed, Arc, RwLock};

use rand::RngCore;

use rsnano_ledger::{AnySet, BlockSource, ConfirmedSet, Ledger, LedgerSet};
use rsnano_messages::{AscPullReqType, BlocksReqPayload, FrontiersReqPayload, HashType};
use rsnano_network::{token_bucket::TokenBucket, Channel, Network, TrafficType};
use rsnano_nullable_clock::SteadyClock;
use rsnano_nullable_random::NullableRngFactory;
use rsnano_types::{Account, BlockHash, HashOrAccount};

use crate::{
    block_processing::BlockProcessorQueue,
    bootstrap::bootstrapper::{
        logic::{BootstrapLogic, BootstrapQueue},
        requesters::{
            priority::{PullCountDecider, PullType, PullTypeDecider},
            stats::BootstrapRequesterStats,
        },
        AscPullQuerySpec, BootstrapConfig,
    },
};

pub(crate) struct QuerySpecFactory {
    config: BootstrapConfig,
    clock: SteadyClock,
    stats: Arc<BootstrapRequesterStats>,
    network: Arc<RwLock<Network>>,
    request_limiter: TokenBucket,
    block_processor_queue: Arc<BlockProcessorQueue>,
    bootstrap_queue: Arc<BootstrapQueue>,
    rng_factory: NullableRngFactory,
    frontiers_limiter: TokenBucket,
    pull_type_decider: PullTypeDecider,
    pull_count_decider: PullCountDecider,
    ledger: Arc<Ledger>,
}

impl QuerySpecFactory {
    pub fn new(
        config: BootstrapConfig,
        stats: Arc<BootstrapRequesterStats>,
        network: Arc<RwLock<Network>>,
        ledger: Arc<Ledger>,
        block_processor_queue: Arc<BlockProcessorQueue>,
        bootstrap_queue: Arc<BootstrapQueue>,
    ) -> Self {
        let pull_type_decider = PullTypeDecider::new(config.optimistic_request_percentage);
        let pull_count_decider = PullCountDecider::new(config.max_pull_count);
        let limiter = TokenBucket::new(config.rate_limit);
        Self {
            clock: SteadyClock::default(),
            stats,
            network,
            request_limiter: limiter,
            block_processor_queue,
            bootstrap_queue,
            rng_factory: NullableRngFactory::default(),
            frontiers_limiter: TokenBucket::new(config.frontier_rate_limit),
            config,
            pull_count_decider,
            pull_type_decider,
            ledger,
        }
    }

    pub fn try_blocks_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
        if state.running_queries.len() >= self.config.max_requests {
            self.stats.queries_overfill.fetch_add(1, Relaxed);
            return None;
        }

        if !self.request_limiter.could_consume(1) {
            self.stats.rate_limit.fetch_add(1, Relaxed);
            return None;
        }

        if !self.block_processor_free() {
            self.stats.wait_block_processor.fetch_add(1, Relaxed);
            return None;
        }
        let query_id = self.rng_factory.rng().next_u64();
        let Some((next_account, next_prio)) = self.bootstrap_queue.next_download_target() else {
            self.stats.wait_next_download.fetch_add(1, Relaxed);
            return None;
        };

        let (head, confirmed_frontier) = self.get_account_info(&next_account);
        let pull_type = self.pull_type_decider.decide_pull_type();
        let pull_start = PullStart::new(pull_type, next_account, head, confirmed_frontier);
        let req_type = AscPullReqType::Blocks(BlocksReqPayload {
            start_type: pull_start.start_type,
            start: pull_start.start,
            count: self.pull_count_decider.pull_count(next_prio),
        });

        let channel = self.acquire_channel(state)?;
        let channel_id = channel.channel_id();
        let query = AscPullQuerySpec {
            query_id,
            channel: channel,
            req_type,
            hash: pull_start.hash,
            account: next_account,
        };
        tracing::trace!(query_id, ?pull_type, "Created pull query spec");

        self.request_limiter.consume(1);
        state.scoring.add_query(channel_id);
        self.bootstrap_queue.download_started(&next_account);

        Some(query)
    }

    pub fn try_dependency_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
        if state.running_queries.len() >= self.config.max_requests {
            self.stats.queries_overfill.fetch_add(1, Relaxed);
            return None;
        }

        if !self.request_limiter.could_consume(1) {
            self.stats.rate_limit.fetch_add(1, Relaxed);
            return None;
        }

        let channel = self.acquire_channel(state)?;
        let channel_id = channel.channel_id();
        let query_id = self.rng_factory.rng().next_u64();
        let Some(next_hash) = self.bootstrap_queue.next_unknown_blocking_hash() else {
            self.stats.wait_dependency_missing.fetch_add(1, Relaxed);
            return None;
        };
        let spec = AscPullQuerySpec {
            query_id,
            channel: channel.clone(),
            req_type: AscPullReqType::account_info_by_hash(next_hash),
            account: Account::ZERO,
            hash: next_hash,
        };
        self.request_limiter.consume(1);
        state.scoring.add_query(channel_id);
        self.bootstrap_queue
            .dependency_account_requested(&spec.hash);
        Some(spec)
    }

    pub fn try_frontier_query(&mut self, state: &mut BootstrapLogic) -> Option<AscPullQuerySpec> {
        let now = self.clock.now();

        if state.running_queries.len() >= self.config.max_requests {
            self.stats.queries_overfill.fetch_add(1, Relaxed);
            return None;
        }

        if state.bootstrap_queue.queue_half_full() {
            return None;
        }
        if !self.frontiers_limiter.could_consume(1) {
            return None;
        }

        if state.frontiers_processor.frontier_checker_overfill() {
            return None;
        }
        let channel = self.acquire_channel(state)?;

        let start = state.frontiers_processor.next(now);
        if !start.is_zero() {
            self.frontiers_limiter.consume(1);
            state.scoring.add_query(channel.channel_id());
            let id = self.rng_factory.rng().next_u64();
            Some(Self::create_frontier_query_spec(&channel, start, id))
        } else {
            None
        }
    }

    fn acquire_channel(&mut self, state: &mut BootstrapLogic) -> Option<Arc<Channel>> {
        let network = self.network.read().unwrap();
        let candidate_channels: Vec<_> = network
            .available_channels(TrafficType::BootstrapRequests)
            .map(|c| c.channel_id())
            .collect();
        let Some(channel_id) = state.scoring.channel(candidate_channels) else {
            self.stats.no_channel.fetch_add(1, Relaxed);
            return None;
        };
        let channel = network.get(channel_id).cloned();
        if channel.is_none() {
            self.stats.no_channel.fetch_add(1, Relaxed);
        }
        channel
    }

    fn block_processor_free(&self) -> bool {
        self.block_processor_queue.queue_len(BlockSource::Bootstrap)
            < self.config.block_processor_threshold
    }

    fn create_frontier_query_spec(
        channel: &Arc<Channel>,
        start: Account,
        query_id: u64,
    ) -> AscPullQuerySpec {
        let request = Self::request_frontiers(start);
        AscPullQuerySpec {
            query_id,
            channel: channel.clone(),
            req_type: request,
            account: Account::ZERO,
            hash: BlockHash::ZERO,
        }
    }

    fn request_frontiers(start: Account) -> AscPullReqType {
        AscPullReqType::Frontiers(FrontiersReqPayload {
            start,
            count: FrontiersReqPayload::MAX_FRONTIERS,
        })
    }

    fn get_account_info(&self, account: &Account) -> (BlockHash, BlockHash) {
        let any = self.ledger.any();
        let account_info = any.get_account(account);
        let head = account_info.map(|i| i.head).unwrap_or_default();

        if let Some(conf_info) = any.confirmed().get_conf_info(account) {
            (head, conf_info.frontier)
        } else {
            (head, BlockHash::ZERO)
        }
    }
}

pub(crate) struct PullStart {
    start: HashOrAccount,
    start_type: HashType,
    hash: BlockHash,
}

impl PullStart {
    pub fn new(
        pull_type: PullType,
        account: Account,
        head: BlockHash,
        confirmed_frontier: BlockHash,
    ) -> Self {
        // Check if the account picked has blocks, if it does, start the pull from the highest block
        if head.is_zero() {
            PullStart::account(account)
        } else {
            match pull_type {
                PullType::Optimistic => PullStart::block(head),
                PullType::Safe => PullStart::safe(account, confirmed_frontier),
            }
        }
    }

    pub fn safe(account: Account, confirmed_frontier: BlockHash) -> Self {
        if confirmed_frontier.is_zero() {
            PullStart::account(account)
        } else {
            PullStart::block(confirmed_frontier)
        }
    }

    pub fn account(account: Account) -> Self {
        Self {
            start: account.into(),
            start_type: HashType::Account,
            hash: BlockHash::ZERO,
        }
    }

    pub fn block(start: BlockHash) -> Self {
        Self {
            start: start.into(),
            start_type: HashType::Block,
            hash: start,
        }
    }
}
