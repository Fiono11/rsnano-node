mod peer_scoring;
mod running_query;
mod running_query_container;

pub(crate) use peer_scoring::PeerScoring;
pub(crate) use running_query::*;
pub(crate) use running_query_container::*;

use std::time::Duration;

use rsnano_messages::AscPullAck;
use rsnano_network::ChannelId;
use rsnano_nullable_clock::Timestamp;
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

use super::BootstrapConfig;

/// Keeps track of all currently running bootstrap queries
pub(crate) struct QueryTracker {
    pub(crate) scoring: PeerScoring,
    pub(crate) running_queries: RunningQueryContainer,
    pub(crate) stopped: bool,
}

impl QueryTracker {
    pub fn new(config: BootstrapConfig) -> Self {
        let mut scoring = PeerScoring::new();
        scoring.set_channel_limit(config.channel_limit);
        Self {
            scoring,
            running_queries: RunningQueryContainer::default(),
            stopped: false,
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

impl ContainerInfoProvider for QueryTracker {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .leaf(
                "tags",
                self.running_queries.len(),
                RunningQueryContainer::ELEMENT_SIZE,
            )
            .node("peers", self.scoring.container_info())
            .finish()
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
