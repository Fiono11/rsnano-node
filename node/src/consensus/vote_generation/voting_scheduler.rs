use std::{collections::HashMap, time::Duration};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, QualifiedRoot};

use crate::consensus::election::{Election, VoteType};

pub(crate) struct VoteTarget {
    pub root: QualifiedRoot,
    pub winner: BlockHash,
    pub vote_type: VoteType,
}

pub(crate) fn vote_target(e: &Election) -> VoteTarget {
    VoteTarget {
        root: e.qualified_root().clone(),
        winner: e.winner().hash(),
        vote_type: e.vote_type(),
    }
}

pub(crate) struct VotingScheduler {
    records: HashMap<QualifiedRoot, VoteRecord>,
    interval: Duration,
}

struct VoteRecord {
    last_non_final: Option<Timestamp>,
    last_final: Option<Timestamp>,
    last_voted_winner: BlockHash,
}

impl VotingScheduler {
    pub fn new(interval: Duration) -> Self {
        Self {
            records: HashMap::new(),
            interval,
        }
    }

    /// Returns true if enough time has passed since the last vote for this election,
    /// or if the winner has changed since the last vote.
    pub fn can_vote(&self, target: &VoteTarget, now: Timestamp) -> bool {
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
            Some(ts) => now >= ts + self.interval,
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

    /// Remove entries whose most recent vote is older than the interval.
    /// Called once per tick to bound memory usage.
    pub fn cleanup(&mut self, now: Timestamp) {
        self.records.retain(|_, record| {
            let last = record.last_non_final.max(record.last_final);
            match last {
                None => false,
                Some(ts) => now < ts + self.interval,
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

    #[test]
    fn can_vote_without_prior_vote() {
        assert!(scheduler().can_vote(&target(VoteType::NonFinal), t(0)));
    }

    #[test]
    fn cannot_vote_before_interval_elapses() {
        let mut s = scheduler();
        s.mark_voted(&target(VoteType::NonFinal), t(0));
        assert!(!s.can_vote(&target(VoteType::NonFinal), t(5)));
    }

    #[test]
    fn can_vote_after_interval_elapses() {
        let mut s = scheduler();
        s.mark_voted(&target(VoteType::NonFinal), t(0));
        assert!(s.can_vote(&target(VoteType::NonFinal), t(15)));
    }

    #[test]
    fn can_vote_immediately_if_winner_changed() {
        let mut s = scheduler();
        s.mark_voted(&target(VoteType::NonFinal), t(0));
        assert!(s.can_vote(&other_winner_target(VoteType::NonFinal), t(1)));
    }

    #[test]
    fn can_vote_final_immediately_after_nonfinal() {
        let mut s = scheduler();
        s.mark_voted(&target(VoteType::NonFinal), t(0));
        // Final vote type not recorded yet, so can vote immediately
        assert!(s.can_vote(&target(VoteType::Final), t(1)));
    }

    #[test]
    fn cleanup_removes_stale_entries() {
        let mut s = scheduler();
        s.mark_voted(&target(VoteType::NonFinal), t(0));
        s.cleanup(t(15));
        // After cleanup the entry is gone, so can_vote returns true
        assert!(s.can_vote(&target(VoteType::NonFinal), t(15)));
    }

    #[test]
    fn cleanup_retains_fresh_entries() {
        let mut s = scheduler();
        s.mark_voted(&target(VoteType::NonFinal), t(0));
        s.cleanup(t(5));
        assert!(!s.can_vote(&target(VoteType::NonFinal), t(5)));
    }

    /*
     * Test helpers
     */

    const INTERVAL: Duration = Duration::from_secs(15);

    fn scheduler() -> VotingScheduler {
        VotingScheduler::new(INTERVAL)
    }

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
}
