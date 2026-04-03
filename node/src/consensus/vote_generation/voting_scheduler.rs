use std::{collections::HashMap, time::Duration};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, QualifiedRoot};

use crate::consensus::election::VoteType;

pub(crate) struct VoteTarget {
    pub root: QualifiedRoot,
    pub winner: BlockHash,
    pub vote_type: VoteType,
}

pub(crate) struct VotingScheduler {
    records: HashMap<QualifiedRoot, VoteRecord>,
}

struct VoteRecord {
    last_non_final: Option<Timestamp>,
    last_final: Option<Timestamp>,
    last_voted_winner: BlockHash,
}

impl VotingScheduler {
    pub fn new() -> Self {
        Self {
            records: HashMap::new(),
        }
    }

    /// Returns true if enough time has passed since the last vote for this election,
    /// or if the winner has changed since the last vote.
    pub fn can_vote(&self, target: &VoteTarget, interval: Duration, now: Timestamp) -> bool {
        let Some(record) = self.records.get(&target.root) else {
            return true;
        };

        if record.last_voted_winner != target.winner {
            return true;
        }

        let last = match target.vote_type {
            VoteType::NonFinal => record.last_non_final,
            VoteType::Final => record.last_final,
        };

        match last {
            None => true,
            Some(ts) => now >= ts + interval,
        }
    }

    pub fn mark_voted(&mut self, target: &VoteTarget, now: Timestamp) {
        let record = self
            .records
            .entry(target.root.clone())
            .or_insert(VoteRecord {
                last_non_final: None,
                last_final: None,
                last_voted_winner: BlockHash::ZERO,
            });

        match target.vote_type {
            VoteType::NonFinal => record.last_non_final = Some(now),
            VoteType::Final => record.last_final = Some(now),
        }
        record.last_voted_winner = target.winner;
    }

    /// Remove entries whose most recent vote is older than `interval`.
    /// Called once per tick to bound memory usage.
    pub fn cleanup(&mut self, now: Timestamp, interval: Duration) {
        self.records.retain(|_, record| {
            let last = record.last_non_final.max(record.last_final);
            match last {
                None => false,
                Some(ts) => now < ts + interval,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::QualifiedRoot;
    use std::time::Duration;

    const INTERVAL: Duration = Duration::from_secs(15);

    fn target(vote_type: VoteType) -> VoteTarget {
        VoteTarget {
            root: QualifiedRoot::new_test_instance(),
            winner: BlockHash::from(1),
            vote_type,
        }
    }

    fn other_winner_target(vote_type: VoteType) -> VoteTarget {
        VoteTarget {
            root: QualifiedRoot::new_test_instance(),
            winner: BlockHash::from(2),
            vote_type,
        }
    }

    fn t(secs: u64) -> Timestamp {
        Timestamp::new_test_instance() + Duration::from_secs(secs)
    }

    /* Tests */

    #[test]
    fn can_vote_without_prior_vote() {
        let scheduler = VotingScheduler::new();
        assert!(scheduler.can_vote(&target(VoteType::NonFinal), INTERVAL, t(0)));
    }

    #[test]
    fn cannot_vote_before_interval_elapses() {
        let mut scheduler = VotingScheduler::new();
        scheduler.mark_voted(&target(VoteType::NonFinal), t(0));
        assert!(!scheduler.can_vote(&target(VoteType::NonFinal), INTERVAL, t(5)));
    }

    #[test]
    fn can_vote_after_interval_elapses() {
        let mut scheduler = VotingScheduler::new();
        scheduler.mark_voted(&target(VoteType::NonFinal), t(0));
        assert!(scheduler.can_vote(&target(VoteType::NonFinal), INTERVAL, t(15)));
    }

    #[test]
    fn can_vote_immediately_if_winner_changed() {
        let mut scheduler = VotingScheduler::new();
        scheduler.mark_voted(&target(VoteType::NonFinal), t(0));
        assert!(scheduler.can_vote(&other_winner_target(VoteType::NonFinal), INTERVAL, t(1)));
    }

    #[test]
    fn can_vote_final_immediately_after_nonfinal() {
        let mut scheduler = VotingScheduler::new();
        scheduler.mark_voted(&target(VoteType::NonFinal), t(0));
        // Final vote type not recorded yet, so can vote immediately
        assert!(scheduler.can_vote(&target(VoteType::Final), INTERVAL, t(1)));
    }

    #[test]
    fn cleanup_removes_stale_entries() {
        let mut scheduler = VotingScheduler::new();
        scheduler.mark_voted(&target(VoteType::NonFinal), t(0));
        scheduler.cleanup(t(15), INTERVAL);
        // After cleanup the entry is gone, so can_vote returns true
        assert!(scheduler.can_vote(&target(VoteType::NonFinal), INTERVAL, t(15)));
    }

    #[test]
    fn cleanup_retains_fresh_entries() {
        let mut scheduler = VotingScheduler::new();
        scheduler.mark_voted(&target(VoteType::NonFinal), t(0));
        scheduler.cleanup(t(5), INTERVAL);
        assert!(!scheduler.can_vote(&target(VoteType::NonFinal), INTERVAL, t(5)));
    }
}
