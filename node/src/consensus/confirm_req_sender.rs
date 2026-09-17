use std::{sync::Arc, time::Duration};

use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::ElectionId;
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{
    ConfirmationSolicitor,
    bounded_hash_map::BoundedHashMap,
    election::{Election, ElectionBehavior},
};

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

    pub fn send_confirm_req(&mut self, solicitor: &mut ConfirmationSolicitor, election: &Election) {
        if self.should_send_confirm_req(election) && solicitor.add(election) {
            self.last_requests.insert(election.id(), self.clock.now());
            self.stats
                .inc(StatType::Election, DetailType::ConfirmationRequest);
        }
    }

    fn should_send_confirm_req(&self, election: &Election) -> bool {
        let now = self.clock.now();
        match self.last_requests.get(&election.id()) {
            Some(last_req) => last_req.elapsed(now) >= Self::confirm_req_interval(election),
            // Every representative votes as soon as it sees a block, so a request
            // only recovers lost votes: leave the proactive votes time to arrive
            // instead of soliciting every election as soon as it becomes active.
            #[cfg(feature = "rai_protocol")]
            None => election.start().elapsed(now) >= election.base_latency() * 2,
            #[cfg(not(feature = "rai_protocol"))]
            None => true,
        }
    }

    /// Calculates time delay between broadcasting confirmation requests
    fn confirm_req_interval(election: &Election) -> Duration {
        // A notarized or timed-out election only collects peers' other
        // certificates, until it finalizes. The epoch closer solicits each
        // such election once right after its decision; this rotation is the
        // slow background that repairs what that reply missed.
        #[cfg(feature = "rai_protocol")]
        if election.has_quorum() || election.is_timed_out() {
            return election.base_latency() * 30;
        }
        match election.behavior() {
            ElectionBehavior::Priority | ElectionBehavior::Manual | ElectionBehavior::Hinted => {
                election.base_latency() * 5
            }
            ElectionBehavior::Optimistic => election.base_latency() * 2,
        }
    }
}
