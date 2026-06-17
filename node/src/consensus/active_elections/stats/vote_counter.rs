use rsnano_utils::stats::{StatsCollection, StatsSource};
use strum::{EnumCount, IntoEnumIterator};

use rsnano_types::VoteDelivery;

#[derive(Default)]
pub(crate) struct VoteCounter {
    votes: u64,
    by_source: [u64; VoteDelivery::COUNT],
}

impl VoteCounter {
    #[allow(dead_code)]
    pub fn votes(&self) -> u64 {
        self.votes
    }

    #[allow(dead_code)]
    pub fn votes_by(&self, source: VoteDelivery) -> u64 {
        self.by_source[source as usize]
    }

    pub fn count(&mut self, source: VoteDelivery) {
        self.votes += 1;
        self.by_source[source as usize] += 1;
    }
}

impl StatsSource for VoteCounter {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert("election", "vote", self.votes);
        for source in VoteDelivery::iter() {
            result.insert(
                "election_vote",
                source.as_str(),
                self.by_source[source as usize],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_counted() {
        let counter = VoteCounter::default();
        assert_eq!(counter.votes(), 0);
        assert_eq!(counter.votes_by(VoteDelivery::Direct), 0);
        assert_eq!(counter.votes_by(VoteDelivery::Replayed), 0);
        assert_eq!(counter.votes_by(VoteDelivery::Forwarded), 0);
    }

    #[test]
    fn count_one_vote() {
        let mut counter = VoteCounter::default();

        counter.count(VoteDelivery::Direct);

        assert_eq!(counter.votes(), 1);
        assert_eq!(counter.votes_by(VoteDelivery::Direct), 1);
        assert_eq!(counter.votes_by(VoteDelivery::Replayed), 0);
        assert_eq!(counter.votes_by(VoteDelivery::Forwarded), 0);
    }

    #[test]
    fn count_multiple_votes() {
        let mut counter = VoteCounter::default();

        counter.count(VoteDelivery::Direct);
        counter.count(VoteDelivery::Direct);
        counter.count(VoteDelivery::Forwarded);

        assert_eq!(counter.votes(), 3);
        assert_eq!(counter.votes_by(VoteDelivery::Direct), 2);
        assert_eq!(counter.votes_by(VoteDelivery::Replayed), 0);
        assert_eq!(counter.votes_by(VoteDelivery::Forwarded), 1);
    }

    #[test]
    fn collect_stats() {
        let mut stats = StatsCollection::new();
        let mut counter = VoteCounter::default();
        counter.count(VoteDelivery::Direct);
        counter.count(VoteDelivery::Direct);
        counter.count(VoteDelivery::Forwarded);

        counter.collect_stats(&mut stats);

        assert_eq!(stats.get("election", "vote"), 3);
        assert_eq!(stats.get("election_vote", "direct"), 2);
        assert_eq!(stats.get("election_vote", "forwarded"), 1);
        assert_eq!(stats.get("election_vote", "replayed"), 0);
    }
}
