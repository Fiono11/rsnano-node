pub mod frontiers_processor;

mod frontier_scan;
mod peer_scoring;
mod running_query;
mod running_query_container;

pub use frontier_scan::{FrontierHeadInfo, FrontierScanConfig};
pub use frontiers_processor::FrontierScanSnapshot;

pub(crate) use frontier_scan::FrontierScan;
pub(crate) use peer_scoring::PeerScoring;
pub(crate) use running_query::*;
pub(crate) use running_query_container::*;

use std::time::Duration;

use rsnano_messages::AscPullAck;
use rsnano_network::ChannelId;
use rsnano_nullable_clock::Timestamp;
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use super::BootstrapConfig;
use frontiers_processor::FrontiersProcessor;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VerifyResult {
    Ok,
    NothingNew,
    Invalid,
}

pub(crate) struct BootstrapLogic {
    pub(crate) scoring: PeerScoring,
    pub(crate) running_queries: RunningQueryContainer,
    pub(crate) stopped: bool,
    pub frontiers_processor: FrontiersProcessor,
}

impl BootstrapLogic {
    pub fn new(config: BootstrapConfig) -> Self {
        let mut scoring = PeerScoring::new();
        scoring.set_channel_limit(config.channel_limit);

        Self {
            scoring,
            running_queries: RunningQueryContainer::default(),
            stopped: false,
            frontiers_processor: FrontiersProcessor::new(config.frontier_scan.clone()),
        }
    }

    pub fn take_running_query_for(
        &mut self,
        response: &AscPullAck,
        channel_id: ChannelId,
    ) -> Result<RunningQuery, ProcessError> {
        // Only process messages that have a known running query
        let Some(query) = self.running_queries.remove(response.id) else {
            return Err(ProcessError::NoRunningQueryFound);
        };

        if !query.is_valid_response_type(response) {
            return Err(ProcessError::InvalidResponseType);
        }

        self.scoring.received_message(channel_id);

        Ok(query)
    }
}

impl ContainerInfoProvider for BootstrapLogic {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .leaf(
                "tags",
                self.running_queries.len(),
                RunningQueryContainer::ELEMENT_SIZE,
            )
            .node("frontiers", self.frontiers_processor.container_info())
            .node("peers", self.scoring.container_info())
            .finish()
    }
}

impl StatsSource for BootstrapLogic {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.frontiers_processor.collect_stats(result);
    }
}

#[derive(Debug)]
pub(crate) enum ProcessError {
    NoRunningQueryFound,
    InvalidResponseType,
    InvalidResponse,
}

pub(crate) struct ProcessInfo {
    pub query_type: QueryType,
    pub response_time: Duration,
}

impl ProcessInfo {
    pub fn new(query: &RunningQuery, now: Timestamp) -> Self {
        Self {
            query_type: query.query_type,
            response_time: query.sent.elapsed(now),
        }
    }
}
