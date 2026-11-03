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
    /// The earliest an election asks again, whatever its urgency (see
    /// `ActiveElectionsConfig::min_request_interval`)
    min_interval: Duration,
}

impl ConfirmReqSender {
    pub(crate) fn new(stats: Arc<Stats>, clock: Arc<SteadyClock>, min_interval: Duration) -> Self {
        Self {
            stats,
            clock,
            last_requests: BoundedHashMap::new(1024 * 32),
            min_interval,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn new_null() -> Self {
        let stats = Arc::new(Stats::default());
        let clock = Arc::new(SteadyClock::new_null());
        Self::new(stats, clock, Duration::ZERO)
    }

    pub fn send_confirm_req(
        &mut self,
        solicitor: &mut ConfirmationSolicitor,
        election: &Election,
        urgency: Urgency,
    ) {
        if self.should_send_confirm_req(election, urgency, self.clock.now())
            && solicitor.add(election)
        {
            self.last_requests.insert(election.id(), self.clock.now());
            self.stats
                .inc(StatType::Election, DetailType::ConfirmationRequest);
        }
    }

    pub(crate) fn should_send_confirm_req(
        &self,
        election: &Election,
        urgency: Urgency,
        now: Timestamp,
    ) -> bool {
        if let Some(last_req) = self.last_requests.get(&election.id()) {
            let interval = Self::confirm_req_interval(election, urgency).max(self.min_interval);
            last_req.elapsed(now) >= interval
        } else {
            true
        }
    }

    #[cfg(test)]
    fn record_request(&mut self, election: &Election, now: Timestamp) {
        self.last_requests.insert(election.id(), now);
    }

    /// Calculates time delay between broadcasting confirmation requests
    fn confirm_req_interval(election: &Election, urgency: Urgency) -> Duration {
        match urgency {
            Urgency::Now => Duration::ZERO,
            Urgency::Soon => election.base_latency(),
            Urgency::Slow => election.base_latency() * 10,
            // Kudzu: an instance without a certificate yet is waiting for a
            // missing candidate or statement; a reply dropped under load is
            // asked for again after one base latency, not five
            #[cfg(feature = "rai_protocol")]
            Urgency::Normal if !election.state().is_terminated() => election.base_latency(),
            Urgency::Normal => match election.behavior() {
                ElectionBehavior::Priority
                | ElectionBehavior::Manual
                | ElectionBehavior::Hinted => election.base_latency() * 5,
                ElectionBehavior::Optimistic => election.base_latency() * 2,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use rsnano_types::SavedBlock;

    use super::*;

    /// Without a minimum the most urgent requests repeat at every tick
    #[test]
    fn an_urgent_request_repeats_at_once_by_default() {
        let election = Election::new_test_instance_with(SavedBlock::new_test_instance());
        let mut sender = ConfirmReqSender::new_null();
        let now = Timestamp::new_test_instance();
        sender.record_request(&election, now);
        assert!(sender.should_send_confirm_req(&election, Urgency::Now, now));
    }

    /// Across wide-area links an election asks again only once a reply to
    /// its last request could have arrived, however urgent it is
    #[test]
    fn the_minimum_interval_holds_back_every_urgency() {
        let election = Election::new_test_instance_with(SavedBlock::new_test_instance());
        let mut sender = ConfirmReqSender::new(
            Arc::new(Stats::default()),
            Arc::new(SteadyClock::new_null()),
            Duration::from_millis(300),
        );
        let now = Timestamp::new_test_instance();
        sender.record_request(&election, now);
        for urgency in [Urgency::Now, Urgency::Soon, Urgency::Slow, Urgency::Normal] {
            assert!(!sender.should_send_confirm_req(
                &election,
                urgency,
                now + Duration::from_millis(299)
            ));
        }
        // The most urgent request has no interval of its own: the minimum
        // is all that holds it
        assert!(sender.should_send_confirm_req(
            &election,
            Urgency::Now,
            now + Duration::from_millis(300)
        ));
    }
}
