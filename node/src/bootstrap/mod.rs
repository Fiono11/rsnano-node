mod block_inspector;
mod bootstrapper;
mod cleanup;
mod requesters;
pub mod responder;
mod response_processor;

pub mod state;
use state::QueryType;
pub use state::{FrontierHeadInfo, FrontierScanConfig};
use std::sync::Arc;

pub use bootstrapper::*;
use rsnano_messages::{AscPullReqType, FrontiersReqPayload, HashType};
use rsnano_network::Channel;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, BlockHash};

trait BootstrapPromise<T> {
    fn poll(&mut self, context: &mut PromiseContext) -> PollResult<T>;
}

enum PollResult<T> {
    Progress,
    Wait,
    Finished(T),
}

pub struct PromiseContext<'a> {
    pub logic: &'a mut state::BootstrapLogic,
    pub now: Timestamp,
    pub id: u64,
}

impl<'a> PromiseContext<'a> {
    pub fn new_test_instance(state: &'a mut state::BootstrapLogic) -> Self {
        Self {
            logic: state,
            now: Timestamp::new_test_instance(),
            id: 123,
        }
    }
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct AscPullQuerySpec {
    pub query_id: u64,
    pub channel: Arc<Channel>,
    pub req_type: AscPullReqType,
    pub account: Account,
    pub hash: BlockHash,
    pub cooldown_account: bool,
}

impl AscPullQuerySpec {
    #[allow(dead_code)]
    pub fn new_test_instance() -> Self {
        Self {
            query_id: 123567,
            req_type: AscPullReqType::Frontiers(FrontiersReqPayload {
                start: 100.into(),
                count: 1000,
            }),
            channel: Arc::new(Channel::new_test_instance()),
            account: Account::from(100),
            hash: BlockHash::from(200),
            cooldown_account: false,
        }
    }

    pub fn query_type(&self) -> QueryType {
        match &self.req_type {
            AscPullReqType::Blocks(b) => match b.start_type {
                HashType::Account => QueryType::BlocksByAccount,
                HashType::Block => QueryType::BlocksByHash,
            },
            AscPullReqType::AccountInfo(_) => QueryType::AccountInfoByHash,
            AscPullReqType::Frontiers(_) => QueryType::Frontiers,
        }
    }
}

#[cfg(test)]
pub(self) fn progress_state<T>(
    requester: &mut impl BootstrapPromise<T>,
    state: &mut state::BootstrapLogic,
) -> PollResult<T> {
    let mut context = PromiseContext {
        logic: state,
        now: Timestamp::new_test_instance(),
        id: 123,
    };

    progress(requester, &mut context)
}

#[cfg(test)]
pub(self) fn progress<T>(
    requester: &mut impl BootstrapPromise<T>,
    context: &mut PromiseContext,
) -> PollResult<T> {
    loop {
        match requester.poll(context) {
            PollResult::Progress => {}
            result => return result,
        }
    }
}
