mod running_query;
mod running_query_container;

pub(crate) use running_query::*;
pub(crate) use running_query_container::*;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use rsnano_messages::AscPullAck;
use rsnano_network::ChannelId;
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{DetailType, StatType, Stats},
};

/// Keeps track of all currently running bootstrap queries
pub(crate) struct QueryTracker {
    logic: Mutex<QueryTrackerLogic>,
    clock: SteadyClock,
}

impl QueryTracker {
    pub fn new(stats: Arc<Stats>) -> Self {
        Self {
            logic: Mutex::new(QueryTrackerLogic::new(stats)),
            clock: SteadyClock::default(),
        }
    }

    #[cfg(test)]
    pub fn new_null() -> Self {
        Self {
            logic: Mutex::new(QueryTrackerLogic::new(Arc::new(Stats::default()))),
            clock: SteadyClock::new_null(),
        }
    }

    pub fn insert(&self, query: RunningQuery) {
        self.logic.lock().unwrap().insert(query);
    }

    #[cfg(test)]
    pub fn front(&self) -> Option<RunningQuery> {
        self.logic.lock().unwrap().front()
    }

    pub fn take_running_query_for(
        &self,
        response: &AscPullAck,
    ) -> Result<RunningQuery, ProcessError> {
        self.logic.lock().unwrap().take_running_query_for(response)
    }

    pub fn timeout(&self) -> Vec<ChannelId> {
        let now = self.clock.now();
        self.logic.lock().unwrap().timeout(now)
    }

    pub fn query_count(&self) -> usize {
        self.logic.lock().unwrap().query_count()
    }
}

impl ContainerInfoProvider for QueryTracker {
    fn container_info(&self) -> ContainerInfo {
        self.logic.lock().unwrap().container_info()
    }
}

pub(crate) struct QueryTrackerLogic {
    stats: Arc<Stats>,
    running_queries: RunningQueryContainer,
}

impl QueryTrackerLogic {
    pub fn new(stats: Arc<Stats>) -> Self {
        Self {
            running_queries: RunningQueryContainer::default(),
            stats,
        }
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
    ) -> Result<RunningQuery, ProcessError> {
        // Only process messages that have a known running query
        let Some(query) = self.running_queries.remove(response.id) else {
            return Err(ProcessError::NoRunningQueryFound);
        };

        if !query.is_valid_response_type(response) {
            return Err(ProcessError::InvalidResponseType);
        }

        Ok(query)
    }

    pub fn timeout(&mut self, now: Timestamp) -> Vec<ChannelId> {
        let mut timed_out_channels = Vec::new();
        let should_timeout = |query: &RunningQuery| query.response_cutoff < now;

        while let Some(front) = self.running_queries.front() {
            if !should_timeout(front) {
                break;
            }

            self.stats.inc(StatType::Bootstrap, DetailType::Timeout);
            self.stats
                .inc(StatType::BootstrapTimeout, front.query_type.into());
            let front = self.running_queries.pop_front().unwrap();
            timed_out_channels.push(front.channel_id);
        }

        timed_out_channels
    }

    pub fn query_count(&self) -> usize {
        self.running_queries.len()
    }
}

impl ContainerInfoProvider for QueryTrackerLogic {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .leaf(
                "running_queries",
                self.running_queries.len(),
                RunningQueryContainer::ELEMENT_SIZE,
            )
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
