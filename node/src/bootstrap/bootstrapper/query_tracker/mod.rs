mod peer_scoring;
mod running_query;
mod running_query_container;

pub(crate) use peer_scoring::PeerScoring;
pub(crate) use running_query::*;
pub(crate) use running_query_container::*;

use std::{sync::Arc, time::Duration};

use rsnano_messages::AscPullAck;
use rsnano_network::ChannelId;
use rsnano_nullable_clock::Timestamp;
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, StatType, Stats},
};

use super::BootstrapConfig;

/// Keeps track of all currently running bootstrap queries
pub(crate) struct QueryTracker {
    stats: Arc<Stats>,
    scoring: PeerScoring,
    running_queries: RunningQueryContainer,
}

impl QueryTracker {
    pub fn new(config: BootstrapConfig, stats: Arc<Stats>) -> Self {
        let mut scoring = PeerScoring::new();
        scoring.set_channel_limit(config.channel_limit);
        Self {
            scoring,
            running_queries: RunningQueryContainer::default(),
            stats,
        }
    }

    pub fn add_query_for_channel(&mut self, channel_id: ChannelId) {
        self.scoring.add_query(channel_id);
    }

    pub fn find_channel(&mut self, candidates: Vec<ChannelId>) -> Option<ChannelId> {
        self.scoring.channel(candidates)
    }

    pub fn insert(&mut self, query: RunningQuery) {
        self.running_queries.insert(query);
    }

    #[cfg(test)]
    pub fn front(&self) -> Option<RunningQuery> {
        self.running_queries.front().cloned()
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

    pub fn timeout(&mut self, now: Timestamp) {
        self.scoring.decay();
        self.erase_timed_out_requests(now);
    }

    pub fn query_count(&self) -> usize {
        self.running_queries.len()
    }

    fn erase_timed_out_requests(&mut self, now: Timestamp) {
        let should_timeout = |query: &RunningQuery| query.response_cutoff < now;

        while let Some(front) = self.running_queries.front() {
            if !should_timeout(front) {
                break;
            }

            self.stats.inc(StatType::Bootstrap, DetailType::Timeout);
            self.stats
                .inc(StatType::BootstrapTimeout, front.query_type.into());
            self.running_queries.pop_front();
        }
    }

    pub fn clean_up_dead_channels(&mut self, dead_channel_ids: &[ChannelId]) {
        self.scoring.clean_up_dead_channels(dead_channel_ids);
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
