use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use rsnano_network::Channel;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use super::channel_waiter::ChannelWaiter;
use crate::bootstrap::bootstrapper::{
    AscPullQuerySpec, BootstrapPromise, PollResult, PromiseContext,
    requesters::channel_waiter::ChannelWaiterStats,
};

pub(super) struct DependencyRequester {
    state: DependencyState,
    stats: Arc<DependencyRequesterStats>,
    channel_waiter: ChannelWaiter,
}

enum DependencyState {
    Initial,
    WaitChannel,
    WaitBlocking(Arc<Channel>),
}

impl DependencyRequester {
    pub(super) fn new(channel_waiter: ChannelWaiter) -> Self {
        Self {
            state: DependencyState::Initial,
            stats: DependencyRequesterStats {
                channel_waiter: channel_waiter.stats(),
                ..Default::default()
            }
            .into(),
            channel_waiter,
        }
    }

    pub(crate) fn stats(&self) -> Arc<DependencyRequesterStats> {
        self.stats.clone()
    }
}

impl BootstrapPromise<AscPullQuerySpec> for DependencyRequester {
    fn poll(&mut self, context: &mut PromiseContext) -> PollResult<AscPullQuerySpec> {
        match self.state {
            DependencyState::Initial => {
                self.stats.loop_count.fetch_add(1, Ordering::Relaxed);
                self.state = DependencyState::WaitChannel;
                PollResult::Progress
            }
            DependencyState::WaitChannel => match self.channel_waiter.poll(context) {
                PollResult::Wait => PollResult::Wait,
                PollResult::Progress => PollResult::Progress,
                PollResult::Finished(channel) => {
                    self.state = DependencyState::WaitBlocking(channel);
                    PollResult::Progress
                }
            },
            DependencyState::WaitBlocking(ref channel) => {
                match context.logic.next_blocking_query(context.id, channel) {
                    Some(spec) => {
                        self.state = DependencyState::Initial;
                        PollResult::Finished(spec)
                    }
                    _ => {
                        self.stats.wait_blocking.fetch_add(1, Ordering::Relaxed);
                        PollResult::Wait
                    }
                }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct DependencyRequesterStats {
    pub loop_count: AtomicU64,
    pub wait_blocking: AtomicU64,
    pub channel_waiter: Arc<ChannelWaiterStats>,
    pub next: AtomicU64,
}

impl StatsSource for DependencyRequesterStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const STAT_NAME: &str = "boot_requester_dep";

        result.insert(STAT_NAME, "loop", self.loop_count.load(Ordering::Relaxed));
        result.insert(
            STAT_NAME,
            "wait_blocking",
            self.wait_blocking.load(Ordering::Relaxed),
        );

        result.insert(
            "bootstrap_next",
            "next_blocking",
            self.next.load(Ordering::Relaxed),
        );

        self.channel_waiter.collect_stats(STAT_NAME, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::bootstrapper::{
        PromiseContext, progress, progress_state, state::BootstrapLogic,
    };
    use rsnano_network::{Network, token_bucket::TokenBucket};
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{Account, BlockHash};
    use std::sync::{Mutex, RwLock};

    #[test]
    fn happy_path() {
        let network = test_network();
        network.write().unwrap().add_test_channel();
        let mut requester = create_test_requester(network);
        let mut state = BootstrapLogic::default();

        let account = Account::from(1);
        let dependency = BlockHash::from(2);
        state.candidate_accounts.priority_up(&account);
        state
            .candidate_accounts
            .block(account, dependency, Timestamp::new_test_instance());

        let result = progress_state(&mut requester, &mut state);

        let PollResult::Finished(spec) = result else {
            panic!("poll did not finish");
        };

        assert_eq!(spec.hash, dependency);
    }

    #[test]
    fn wait_channel() {
        let network = test_network();
        let mut requester = create_test_requester(network.clone());
        let mut state = BootstrapLogic::default();
        let mut context = PromiseContext::new_test_instance(&mut state);

        let result = progress(&mut requester, &mut context);
        assert!(matches!(result, PollResult::Wait));
        assert!(matches!(requester.state, DependencyState::WaitChannel));

        network.write().unwrap().add_test_channel();
        let result = requester.poll(&mut context);
        assert!(matches!(result, PollResult::Progress));
        assert!(matches!(requester.state, DependencyState::WaitBlocking(_)));
    }

    #[test]
    fn wait_dependency() {
        let network = test_network();
        network.write().unwrap().add_test_channel();
        let mut requester = create_test_requester(network);
        let mut state = BootstrapLogic::default();

        let result = progress_state(&mut requester, &mut state);
        assert!(matches!(result, PollResult::Wait));
        assert!(matches!(requester.state, DependencyState::WaitBlocking(_)));

        let account = Account::from(1);
        let dependency = BlockHash::from(2);
        state.candidate_accounts.priority_up(&account);
        state
            .candidate_accounts
            .block(account, dependency, Timestamp::new_test_instance());

        let mut context = PromiseContext::new_test_instance(&mut state);
        let result = requester.poll(&mut context);
        assert!(matches!(result, PollResult::Finished(_)));
        assert!(matches!(requester.state, DependencyState::Initial));
    }

    fn create_test_requester(network: Arc<RwLock<Network>>) -> DependencyRequester {
        let limiter = Arc::new(Mutex::new(TokenBucket::new(1024)));
        let channel_waiter = ChannelWaiter::new(network, limiter, 1024);
        DependencyRequester::new(channel_waiter)
    }

    fn test_network() -> Arc<RwLock<Network>> {
        Arc::new(RwLock::new(Network::new_test_instance()))
    }
}
