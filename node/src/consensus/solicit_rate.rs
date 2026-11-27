use std::time::Duration;

use rsnano_nullable_clock::Timestamp;

/// RAI: how many elections a node asks about per second, adapted to the
/// votes it can take in. Every request goes to every peered representative,
/// and a terminated instance's reply carries its certificates, so asking
/// about one election brings back tens of votes; at twelve seats the fixed
/// cap of 5000 requests per second asked for about a million votes per
/// second. A node whose vote queue overflows cannot take the answers in, so
/// its elections stay open, so it asks again: in the twelve-seat runs with
/// forks that loop sent tens of millions of votes, dropped hundreds of
/// millions at full send queues, and held closes for minutes.
///
/// The rate halves whenever this node dropped votes since the last tick (at
/// most once per `CUT_SPACING`, so that one burst is not counted at every
/// tick while its answers are still in flight) and grows back by
/// `INCREASE_PER_SECOND` while nothing is dropped. A node that keeps up
/// never drops a vote and asks at `MAX_RATE`, as before.
pub(crate) struct SolicitRate {
    /// Requests per second currently allowed
    rate: f64,
    /// Requests that may go now; at most one second's worth
    tokens: f64,
    last_refill: Option<Timestamp>,
    last_cut: Option<Timestamp>,
}

impl SolicitRate {
    /// Under a load of 2000 blocks/s a node keeping up asks about 1700
    /// elections per second
    pub(crate) const MAX_RATE: f64 = 5000.0;
    /// Enough for the instances an ending epoch waits for to be asked about
    /// every few seconds while the node is overloaded
    pub(crate) const MIN_RATE: f64 = 250.0;
    /// From the floor back to the cap in about five seconds without drops
    pub(crate) const INCREASE_PER_SECOND: f64 = 1000.0;
    /// About one round trip across regions with the replies queued behind it
    pub(crate) const CUT_SPACING: Duration = Duration::from_millis(500);

    pub(crate) fn new() -> Self {
        Self {
            rate: Self::MAX_RATE,
            tokens: Self::MAX_RATE,
            last_refill: None,
            last_cut: None,
        }
    }

    /// Requests per second currently allowed
    pub(crate) fn rate(&self) -> f64 {
        self.rate
    }

    /// Adjusts the rate to whether this node dropped votes since the last call
    pub(crate) fn observe(&mut self, dropped_votes: bool, now: Timestamp) {
        let elapsed = self.refill(now);
        if dropped_votes {
            if self
                .last_cut
                .is_none_or(|cut| cut.elapsed(now) >= Self::CUT_SPACING)
            {
                self.rate = (self.rate / 2.0).max(Self::MIN_RATE);
                self.tokens = self.tokens.min(self.rate);
                self.last_cut = Some(now);
            }
        } else {
            self.rate =
                (self.rate + elapsed.as_secs_f64() * Self::INCREASE_PER_SECOND).min(Self::MAX_RATE);
        }
    }

    /// How many of `wanted` requests may go now
    pub(crate) fn take(&mut self, wanted: usize, now: Timestamp) -> usize {
        self.refill(now);
        let granted = (self.tokens.floor() as usize).min(wanted);
        self.tokens -= granted as f64;
        granted
    }

    /// Adds the tokens earned since the last refill at the rate in force;
    /// returns the time since then
    fn refill(&mut self, now: Timestamp) -> Duration {
        let elapsed = self
            .last_refill
            .map_or(Duration::ZERO, |last| last.elapsed(now));
        self.last_refill = Some(now);
        self.tokens = (self.tokens + elapsed.as_secs_f64() * self.rate).min(self.rate);
        elapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_that_drops_nothing_asks_at_the_full_rate() {
        let mut rate = SolicitRate::new();
        let mut now = Timestamp::new_test_instance();
        assert_eq!(rate.take(100_000, now), 5000);
        for _ in 0..10 {
            now += Duration::from_millis(100);
            rate.observe(false, now);
        }
        assert_eq!(rate.rate(), SolicitRate::MAX_RATE);
        assert_eq!(rate.take(100_000, now), 5000);
    }

    #[test]
    fn dropped_votes_halve_the_rate_down_to_the_floor() {
        let mut rate = SolicitRate::new();
        let mut now = Timestamp::new_test_instance();
        rate.observe(true, now);
        assert_eq!(rate.rate(), 2500.0);
        for _ in 0..10 {
            now += SolicitRate::CUT_SPACING;
            rate.observe(true, now);
        }
        assert_eq!(rate.rate(), SolicitRate::MIN_RATE);
    }

    #[test]
    fn a_cut_takes_away_the_tokens_above_the_new_rate() {
        let mut rate = SolicitRate::new();
        let now = Timestamp::new_test_instance();
        rate.observe(true, now);
        assert_eq!(rate.take(100_000, now), 2500);
    }

    #[test]
    fn drops_within_the_spacing_of_a_cut_count_once() {
        let mut rate = SolicitRate::new();
        let now = Timestamp::new_test_instance();
        rate.observe(true, now);
        rate.observe(true, now + Duration::from_millis(100));
        rate.observe(true, now + Duration::from_millis(400));
        assert_eq!(rate.rate(), 2500.0);
        rate.observe(true, now + SolicitRate::CUT_SPACING);
        assert_eq!(rate.rate(), 1250.0);
    }

    #[test]
    fn the_rate_grows_back_while_nothing_is_dropped() {
        let mut rate = SolicitRate::new();
        let mut now = Timestamp::new_test_instance();
        cut_to_the_floor(&mut rate, &mut now);
        rate.observe(false, now);
        now += Duration::from_secs(2);
        rate.observe(false, now);
        assert_eq!(rate.rate(), SolicitRate::MIN_RATE + 2000.0);
        now += Duration::from_secs(10);
        rate.observe(false, now);
        assert_eq!(rate.rate(), SolicitRate::MAX_RATE);
    }

    #[test]
    fn tokens_accrue_at_the_reduced_rate() {
        let mut rate = SolicitRate::new();
        let mut now = Timestamp::new_test_instance();
        cut_to_the_floor(&mut rate, &mut now);
        rate.take(100_000, now);
        now += Duration::from_millis(400);
        assert_eq!(rate.take(100_000, now), 100);
    }

    /* Test helpers */

    /// Drops at every cut spacing until the rate is at its floor; the last
    /// cut is at `now`
    fn cut_to_the_floor(rate: &mut SolicitRate, now: &mut Timestamp) {
        rate.observe(true, *now);
        while rate.rate() > SolicitRate::MIN_RATE {
            *now += SolicitRate::CUT_SPACING;
            rate.observe(true, *now);
        }
    }
}
