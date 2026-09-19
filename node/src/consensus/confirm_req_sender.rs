use std::{sync::Arc, time::Duration};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{
    ConfirmationSolicitor,
    bounded_hash_map::BoundedHashMap,
    election::{Election, ElectionBehavior, ElectionId},
};

/// How soon an election is asked about again
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Urgency {
    Normal,
    /// RAI: an instance of an epoch this node has left which settled without
    /// a block: asked about now and then, so that a replica which missed it
    /// joins it and agrees on the epoch's state
    Slow,
    /// RAI: an instance of an epoch this node has left which has not settled
    /// yet: its epoch's close waits for it
    Soon,
    /// RAI: an instance the ending epoch waits for
    Now,
}

pub(crate) struct ConfirmReqSender {
    stats: Arc<Stats>,
    last_requests: BoundedHashMap<ElectionId, Timestamp>,
    clock: Arc<SteadyClock>,
}

impl ConfirmReqSender {
    pub(crate) fn new(stats: Arc<Stats>, clock: Arc<SteadyClock>) -> Self {
        Self {
            stats,
            clock,
            last_requests: BoundedHashMap::new(1024 * 32),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn new_null() -> Self {
        let stats = Arc::new(Stats::default());
        let clock = Arc::new(SteadyClock::new_null());
        Self::new(stats, clock)
    }

    pub fn send_confirm_req(
        &mut self,
        solicitor: &mut ConfirmationSolicitor,
        election: &Election,
        urgency: Urgency,
    ) {
        if self.should_send_confirm_req(election, urgency) && solicitor.add(election) {
            self.last_requests.insert(election.id(), self.clock.now());
            self.stats
                .inc(StatType::Election, DetailType::ConfirmationRequest);
        }
    }

    fn should_send_confirm_req(&self, election: &Election, urgency: Urgency) -> bool {
        if let Some(last_req) = self.last_requests.get(&election.id()) {
            last_req.elapsed(self.clock.now()) >= Self::confirm_req_interval(election, urgency)
        } else {
            true
        }
    }

    /// Calculates time delay between broadcasting confirmation requests
    fn confirm_req_interval(election: &Election, urgency: Urgency) -> Duration {
        match urgency {
            Urgency::Now => Duration::ZERO,
            Urgency::Soon => election.base_latency(),
            Urgency::Slow => election.base_latency() * 10,
            Urgency::Normal => match election.behavior() {
                ElectionBehavior::Priority
                | ElectionBehavior::Manual
                | ElectionBehavior::Hinted => election.base_latency() * 5,
                ElectionBehavior::Optimistic => election.base_latency() * 2,
            },
        }
    }
}
