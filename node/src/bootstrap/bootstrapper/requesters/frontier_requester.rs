use std::sync::Arc;

use rsnano_messages::{AscPullReqType, FrontiersReqPayload};
use rsnano_network::{Channel, token_bucket::TokenBucket};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Account, BlockHash};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::channel_waiter::ChannelWaiter;
use crate::bootstrap::bootstrapper::{
    AscPullQuerySpec, BootstrapPromise, PollResult, PromiseContext,
};

/// Creates frontier requests as specified by the frontier scanner
pub(crate) struct FrontierRequester {
    state: FrontierState,
    stats: Arc<Stats>,
    clock: Arc<SteadyClock>,
    frontiers_limiter: TokenBucket,
    channel_waiter: ChannelWaiter,
}

enum FrontierState {
    Initial,
    WaitBootstrapQueue,
    WaitLimiter,
    WaitAckProcessor,
    WaitChannel,
    WaitFrontier(Arc<Channel>),
}

impl FrontierRequester {
    pub(crate) fn new(
        stats: Arc<Stats>,
        clock: Arc<SteadyClock>,
        rate_limit: usize,
        channel_waiter: ChannelWaiter,
    ) -> Self {
        Self {
            state: FrontierState::Initial,
            stats,
            clock,
            frontiers_limiter: TokenBucket::new(rate_limit),
            channel_waiter,
        }
    }

    fn create_query_spec(
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
            cooldown_account: false,
        }
    }

    fn request_frontiers(start: Account) -> AscPullReqType {
        AscPullReqType::Frontiers(FrontiersReqPayload {
            start,
            count: FrontiersReqPayload::MAX_FRONTIERS,
        })
    }
}

impl BootstrapPromise<AscPullQuerySpec> for FrontierRequester {
    fn poll(&mut self, context: &mut PromiseContext) -> PollResult<AscPullQuerySpec> {
        match self.state {
            FrontierState::Initial => {
                self.stats
                    .inc(StatType::Bootstrap, DetailType::LoopFrontiers);
                self.state = FrontierState::WaitBootstrapQueue;
                return PollResult::Progress;
            }
            FrontierState::WaitBootstrapQueue => {
                if !context.logic.bootstrap_queue.priority_half_full() {
                    self.state = FrontierState::WaitLimiter;
                    return PollResult::Progress;
                }
            }
            FrontierState::WaitLimiter => {
                if self.frontiers_limiter.try_consume(1, context.now) {
                    self.state = FrontierState::WaitAckProcessor;
                    return PollResult::Progress;
                }
            }
            FrontierState::WaitAckProcessor => {
                if !context
                    .logic
                    .frontiers_processor
                    .frontier_checker_overfill()
                {
                    self.state = FrontierState::WaitChannel;
                    return PollResult::Progress;
                }
            }
            FrontierState::WaitChannel => match self.channel_waiter.poll(context) {
                PollResult::Wait => return PollResult::Wait,
                PollResult::Progress => return PollResult::Progress,
                PollResult::Finished(channel) => {
                    self.state = FrontierState::WaitFrontier(channel);
                    return PollResult::Progress;
                }
            },
            FrontierState::WaitFrontier(ref channel) => {
                let now = self.clock.now();
                let start = context.logic.frontiers_processor.next(now);
                if !start.is_zero() {
                    self.stats
                        .inc(StatType::BootstrapNext, DetailType::NextFrontier);
                    let spec = Self::create_query_spec(channel, start, context.id);
                    self.state = FrontierState::Initial;
                    return PollResult::Finished(spec);
                } else {
                    self.stats
                        .inc(StatType::BootstrapFrontierScan, DetailType::NextNone);
                }
            }
        }
        PollResult::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::bootstrapper::{
        BootstrapConfig, progress, progress_state,
        state::{BootstrapLogic, BootstrapQueueConfig, FrontierScan},
    };
    use rsnano_network::Network;
    use std::sync::{Mutex, RwLock};

    #[test]
    fn happy_path() {
        let (mut requester, network) = create_test_requester();
        let mut state = BootstrapLogic::default();
        network.write().unwrap().add_test_channel();
        let mut context = PromiseContext::new_test_instance(&mut state);

        let PollResult::Finished(result) = progress(&mut requester, &mut context) else {
            panic!("promise did not finish!")
        };

        assert!(matches!(result.req_type, AscPullReqType::Frontiers(_)));
        assert_eq!(result.query_id, context.id);
    }

    #[test]
    fn wait_bootstrap_queue() {
        let (mut requester, _) = create_test_requester();
        let mut state = state_with_max_priorities(1);
        let mut context = PromiseContext::new_test_instance(&mut state);

        // Fill up bootstrap queue
        context.logic.bootstrap_queue.priority_up(&Account::from(1));

        // Should wait because bootstrap queue is not full enough
        let result = progress(&mut requester, &mut context);
        assert!(matches!(result, PollResult::Wait));
        assert!(matches!(requester.state, FrontierState::WaitBootstrapQueue));

        // Running again continues waiting
        let result = requester.poll(&mut context);
        assert!(matches!(result, PollResult::Wait));

        // If the accounts are cleared, continue
        context.logic.bootstrap_queue.clear();
        let result = requester.poll(&mut context);
        assert!(matches!(result, PollResult::Progress));
        assert!(matches!(requester.state, FrontierState::WaitLimiter));
    }

    #[test]
    fn wait_limiter() {
        let (mut requester, _) = create_test_requester();
        let mut state = BootstrapLogic::default();
        let mut context = PromiseContext::new_test_instance(&mut state);

        // Should wait because rate limit reached
        requester
            .frontiers_limiter
            .try_consume(TEST_RATE_LIMIT, context.now);

        let result = progress(&mut requester, &mut context);
        assert!(matches!(result, PollResult::Wait));
        assert!(matches!(requester.state, FrontierState::WaitLimiter));

        // Running again continues waiting
        let result = requester.poll(&mut context);
        assert!(matches!(result, PollResult::Wait));

        // Continue when the limiter is emptied
        requester.frontiers_limiter.reset();
        let result = requester.poll(&mut context);
        assert!(matches!(result, PollResult::Progress));
        assert!(matches!(requester.state, FrontierState::WaitAckProcessor));
    }

    #[test]
    fn wait_channel() {
        let (mut requester, network) = create_test_requester();
        let mut state = BootstrapLogic::default();
        let mut context = PromiseContext::new_test_instance(&mut state);

        let result = progress(&mut requester, &mut context);
        assert!(matches!(result, PollResult::Wait));
        assert!(matches!(requester.state, FrontierState::WaitChannel));

        // Running again continues waiting
        let result = requester.poll(&mut context);
        assert!(matches!(result, PollResult::Wait));

        network.write().unwrap().add_test_channel();
        let result = requester.poll(&mut context);
        assert!(matches!(result, PollResult::Progress));
    }

    #[test]
    fn finish_request() {
        let (mut requester, network) = create_test_requester();
        let mut state = BootstrapLogic::default();
        network.write().unwrap().add_test_channel();

        let PollResult::Finished(spec) = progress_state(&mut requester, &mut state) else {
            panic!("did not finish");
        };

        let AscPullReqType::Frontiers(frontiers) = spec.req_type else {
            panic!("not a frontier request");
        };

        assert_eq!(frontiers.start, Account::from(1));
        assert!(matches!(requester.state, FrontierState::Initial));
    }

    #[test]
    fn wait_when_frontier_scan_rate_limited() {
        let (mut requester, network) = create_test_requester();
        let mut state = BootstrapLogic::default();
        network.write().unwrap().add_test_channel();
        state.frontiers_processor.frontier_scan = FrontierScan::new_test_instance_blocked();

        let result = progress_state(&mut requester, &mut state);

        assert!(matches!(result, PollResult::Wait));
        assert!(matches!(requester.state, FrontierState::WaitFrontier(_)));
    }

    // Test helpers:

    const TEST_RATE_LIMIT: usize = 1000;

    fn create_test_requester() -> (FrontierRequester, Arc<RwLock<Network>>) {
        let stats = Arc::new(Stats::default());
        let network = Arc::new(RwLock::new(Network::new_test_instance()));
        let limiter = Arc::new(Mutex::new(TokenBucket::new(1024)));
        let waiter = ChannelWaiter::new(network.clone(), limiter, 1024);
        let clock = Arc::new(SteadyClock::new_null());
        let requester = FrontierRequester::new(stats.clone(), clock, TEST_RATE_LIMIT, waiter);
        (requester, network)
    }

    fn state_with_max_priorities(max: usize) -> BootstrapLogic {
        let config = BootstrapConfig {
            bootstrap_queue: BootstrapQueueConfig {
                max_prioritized_accounts: max,
                ..Default::default()
            },
            ..Default::default()
        };
        BootstrapLogic::new(config)
    }
}
