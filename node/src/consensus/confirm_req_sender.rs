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
    /// RAI: held at the gate of its epoch: the validators withhold their first
    /// votes until the epoch before closes, so there is nothing to ask for.
    /// Thousands of them asking several times a second overfilled every
    /// request aggregator and crowded out the close votes they wait for.
    Held,
}

/// An election's last request, and how many requests in a row brought no
/// new vote
#[derive(Clone, Copy)]
struct LastRequest {
    at: Timestamp,
    fruitless: u32,
}

pub(crate) struct ConfirmReqSender {
    stats: Arc<Stats>,
    last_requests: BoundedHashMap<ElectionId, LastRequest>,
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
        let now = self.clock.now();
        if self.should_send_confirm_req(election, urgency, now) && solicitor.add(election) {
            self.record_request(election, now);
            self.stats
                .inc(StatType::Election, DetailType::ConfirmationRequest);
            if is_contested(election) {
                self.stats
                    .inc(StatType::Election, DetailType::ContestedConfirmationRequest);
            }
        }
    }

    pub(crate) fn should_send_confirm_req(
        &self,
        election: &Election,
        urgency: Urgency,
        now: Timestamp,
    ) -> bool {
        if let Some(last) = self.last_requests.get(&election.id()) {
            let interval = Self::confirm_req_interval(election, urgency).max(self.min_interval);
            let wait = Self::request_wait(
                interval,
                is_contested(election),
                Self::backs_off(election, urgency),
                last.fruitless,
            );
            last.at.elapsed(now) >= wait
        } else {
            urgency != Urgency::Held
        }
    }

    /// RAI: a contested instance keeps one pace whatever arrives: a Byzantine
    /// member's votes for both blocks would otherwise bring back the pace of
    /// every fork it splits, and only its epoch's checkpoint can decide those.
    /// It still asks, so that a statement lost in transit comes in the end.
    /// Non-fork instances are not affected, nor is a fork's latency a concern:
    /// what matters of a fork is that it settles.
    fn request_wait(
        interval: Duration,
        contested: bool,
        backs_off: bool,
        fruitless: u32,
    ) -> Duration {
        if contested {
            interval.max(Self::LONGEST_BACKOFF)
        } else if backs_off {
            interval
                .saturating_mul(1 << fruitless.min(Self::MAX_DOUBLINGS))
                .min(Self::LONGEST_BACKOFF.max(interval))
        } else {
            interval
        }
    }

    /// The longest wait a fruitless solicitation backs off to, and the wait
    /// of a contested instance
    const LONGEST_BACKOFF: Duration = Duration::from_secs(8);
    const MAX_DOUBLINGS: u32 = 8;

    /// RAI: an instance collecting evidence - terminated, or of an epoch this
    /// node left - asks again after twice the wait once its last request
    /// brought no new vote, and at its own pace again once one arrives. A
    /// Byzantine member voting for both blocks of every fork leaves thousands
    /// of instances that cannot finalize; asking each of them every few
    /// hundred milliseconds for minutes overfilled every request aggregator
    /// by tens of millions of hashes. Active elections and the instances the
    /// ending epoch waits for keep their pace.
    fn backs_off(election: &Election, urgency: Urgency) -> bool {
        match urgency {
            Urgency::Now | Urgency::Held => false,
            Urgency::Soon | Urgency::Slow => true,
            Urgency::Normal => election.state().is_terminated(),
        }
    }

    fn record_request(&mut self, election: &Election, now: Timestamp) {
        let fruitless = match self.last_requests.get(&election.id()) {
            Some(last) if election.last_vote() <= last.at => last.fruitless + 1,
            _ => 0,
        };
        self.last_requests
            .insert(election.id(), LastRequest { at: now, fruitless });
    }

    /// Calculates time delay between broadcasting confirmation requests
    fn confirm_req_interval(election: &Election, urgency: Urgency) -> Duration {
        match urgency {
            Urgency::Now => Duration::ZERO,
            Urgency::Held => Self::LONGEST_BACKOFF,
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

/// RAI: an instance that terminated holding more than one candidate: nothing
/// but its epoch's checkpoint can decide it once its votes are split
fn is_contested(election: &Election) -> bool {
    #[cfg(feature = "rai_protocol")]
    {
        election.state().is_terminated() && election.candidate_blocks().len() > 1
    }
    #[cfg(not(feature = "rai_protocol"))]
    {
        let _ = election;
        false
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

    /// An instance collecting evidence whose request brought no new vote
    /// waits twice as long before the next one
    #[test]
    fn a_fruitless_request_doubles_the_wait_of_an_instance_collecting_evidence() {
        let election = Election::new_test_instance_with(SavedBlock::new_test_instance());
        let mut sender = ConfirmReqSender::new_null();
        let start = Timestamp::new_test_instance();
        sender.record_request(&election, start);
        let second = start + Duration::from_secs(1);
        sender.record_request(&election, second);
        assert!(!sender.should_send_confirm_req(
            &election,
            Urgency::Soon,
            second + Duration::from_millis(1999)
        ));
        assert!(sender.should_send_confirm_req(
            &election,
            Urgency::Soon,
            second + Duration::from_secs(2)
        ));
    }

    #[test]
    fn a_new_vote_brings_the_pace_back() {
        let block = SavedBlock::new_test_instance();
        let start = Timestamp::new_test_instance();
        let election = Election::new_test_instance_with(block.clone());
        let mut sender = ConfirmReqSender::new_null();
        sender.record_request(&election, start);
        sender.record_request(&election, start + Duration::from_secs(1));
        // The same instance, a vote having reached it after the last request
        let voted = Election::new(
            block,
            election.epoch(),
            election.behavior(),
            election.base_latency(),
            start + Duration::from_millis(1500),
        );
        let third = start + Duration::from_secs(2);
        sender.record_request(&voted, third);
        assert!(sender.should_send_confirm_req(
            &voted,
            Urgency::Soon,
            third + Duration::from_secs(1)
        ));
    }

    #[test]
    fn an_instance_held_at_the_gate_does_not_start_asking() {
        let election = Election::new_test_instance_with(SavedBlock::new_test_instance());
        let sender = ConfirmReqSender::new_null();
        let now = Timestamp::new_test_instance();
        assert!(!sender.should_send_confirm_req(&election, Urgency::Held, now));
        // Released, it asks at once
        assert!(sender.should_send_confirm_req(&election, Urgency::Normal, now));
    }

    #[test]
    fn an_instance_held_after_asking_waits_the_longest() {
        let election = Election::new_test_instance_with(SavedBlock::new_test_instance());
        let mut sender = ConfirmReqSender::new_null();
        let now = Timestamp::new_test_instance();
        sender.record_request(&election, now);
        assert!(!sender.should_send_confirm_req(
            &election,
            Urgency::Held,
            now + Duration::from_millis(7999)
        ));
        assert!(sender.should_send_confirm_req(
            &election,
            Urgency::Held,
            now + Duration::from_secs(8)
        ));
    }

    /// A vote resets `fruitless` to 0: a contested instance keeps its wait
    #[test]
    fn a_new_vote_does_not_bring_back_the_pace_of_a_contested_instance() {
        let interval = Duration::from_millis(300);
        for fruitless in [0, 1, 8] {
            assert_eq!(
                ConfirmReqSender::request_wait(interval, true, true, fruitless),
                Duration::from_secs(8)
            );
        }
    }

    #[test]
    fn a_contested_instance_still_asks_again() {
        let election = Election::new_test_instance_with(SavedBlock::new_test_instance());
        let wait = ConfirmReqSender::request_wait(Duration::from_millis(300), true, true, 0);
        let mut sender = ConfirmReqSender::new_null();
        let now = Timestamp::new_test_instance();
        sender.record_request(&election, now);
        assert!(sender.should_send_confirm_req(&election, Urgency::Soon, now + wait));
    }

    #[test]
    fn an_instance_that_is_not_contested_backs_off_as_before() {
        let interval = Duration::from_millis(300);
        assert_eq!(
            ConfirmReqSender::request_wait(interval, false, true, 0),
            interval
        );
        assert_eq!(
            ConfirmReqSender::request_wait(interval, false, true, 2),
            interval * 4
        );
        assert_eq!(
            ConfirmReqSender::request_wait(interval, false, true, 8),
            Duration::from_secs(8)
        );
        assert_eq!(
            ConfirmReqSender::request_wait(interval, false, false, 8),
            interval
        );
    }

    #[test]
    fn active_elections_and_the_ending_epoch_keep_their_pace() {
        let election = Election::new_test_instance_with(SavedBlock::new_test_instance());
        let mut sender = ConfirmReqSender::new_null();
        let start = Timestamp::new_test_instance();
        let mut at = start;
        for _ in 0..4 {
            at = at + Duration::from_secs(10);
            sender.record_request(&election, at);
        }
        let normal = ConfirmReqSender::confirm_req_interval(&election, Urgency::Normal);
        assert!(sender.should_send_confirm_req(&election, Urgency::Normal, at + normal));
        assert!(sender.should_send_confirm_req(&election, Urgency::Now, at));
    }
}
