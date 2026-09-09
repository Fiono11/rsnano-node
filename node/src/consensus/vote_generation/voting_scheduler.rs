#[cfg(feature = "rai_protocol")]
use rustc_hash::FxHashMap as HashMap;
#[cfg(not(feature = "rai_protocol"))]
use std::collections::HashMap;
use std::{collections::VecDeque, mem::size_of, time::Duration};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, ElectionId};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

use crate::consensus::election::{Election, VoteType};

pub(crate) struct VoteTarget {
    pub root: ElectionId,
    pub winner: BlockHash,
    pub vote_type: VoteType,
}

pub(crate) fn vote_target(e: &Election) -> VoteTarget {
    VoteTarget {
        root: e.id(),
        winner: e.winner().hash(),
        vote_type: e.vote_type(),
    }
}

#[cfg(feature = "rai_protocol")]
type ScheduleKey = (ElectionId, BlockHash);
#[cfg(not(feature = "rai_protocol"))]
type ScheduleKey = ElectionId;

fn schedule_key(target: &VoteTarget) -> ScheduleKey {
    #[cfg(feature = "rai_protocol")]
    {
        (target.root.clone(), target.winner)
    }
    #[cfg(not(feature = "rai_protocol"))]
    {
        target.root.clone()
    }
}

pub(crate) struct VotingScheduler {
    records: HashMap<ScheduleKey, VoteRecord>,
    expiry_queue: VecDeque<(Timestamp, ScheduleKey)>,
    interval: Duration,
}

struct VoteRecord {
    last_non_final: Option<Timestamp>,
    last_final: Option<Timestamp>,
    last_voted_winner: BlockHash,
    last_voted: Timestamp,
}

impl VotingScheduler {
    pub fn new(interval: Duration) -> Self {
        Self {
            records: HashMap::default(),
            expiry_queue: VecDeque::new(),
            interval,
        }
    }

    /// Returns true if enough time has passed since the last vote for this election,
    /// or if the winner has changed since the last vote.
    pub fn can_vote(&self, target: &VoteTarget, now: Timestamp) -> bool {
        let Some(record) = self.records.get(&schedule_key(target)) else {
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
            .entry(schedule_key(target))
            .or_insert(VoteRecord {
                last_non_final: None,
                last_final: None,
                last_voted_winner: BlockHash::ZERO,
                last_voted: now,
            });

        debug_assert!(now >= record.last_voted);

        match target.vote_type {
            VoteType::NonFinal => record.last_non_final = Some(now),
            VoteType::Final => record.last_final = Some(now),
        }
        record.last_voted_winner = target.winner;
        record.last_voted = now;

        self.expiry_queue.push_back((now, schedule_key(target)));
    }

    /// Remove entries whose most recent vote is older than the interval.
    /// Called once per tick to bound memory usage.
    pub fn cleanup(&mut self, now: Timestamp) {
        while let Some(&(ts, ref root)) = self.expiry_queue.front() {
            if now < ts + self.interval {
                break;
            }
            let root = root.clone();
            self.expiry_queue.pop_front();
            if let Some(record) = self.records.get(&root) {
                if record.last_voted == ts {
                    self.records.remove(&root);
                }
            }
        }
    }
}

impl ContainerInfoProvider for VotingScheduler {
    fn container_info(&self) -> ContainerInfo {
        [
            ("records", self.records.len(), size_of::<VoteRecord>()),
            (
                "expiry_queue",
                self.expiry_queue.len(),
                size_of::<(Timestamp, ScheduleKey)>(),
            ),
        ]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::ElectionId;
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

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn alternating_candidates_do_not_bypass_the_retry_interval() {
        let mut s = scheduler();
        let a = target(VoteType::NonFinal);
        let b = other_winner_target(VoteType::NonFinal);
        s.mark_voted(&a, t(0));
        assert!(s.can_vote(&b, t(0)));
        s.mark_voted(&b, t(0));
        assert!(!s.can_vote(&a, t(1)));
        assert!(!s.can_vote(&b, t(1)));
        // Final-phase progress does not wait for the non-final retry interval.
        assert!(s.can_vote(&target(VoteType::Final), t(1)));
        s.cleanup(t(15));
        assert!(s.can_vote(&a, t(15)));
        assert!(s.can_vote(&b, t(15)));
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
            root: ElectionId::new(rsnano_types::QualifiedRoot::new_test_instance(), 0),
            winner: BlockHash::from(1),
            vote_type,
        }
    }

    fn other_winner_target(vote_type: VoteType) -> VoteTarget {
        VoteTarget {
            root: ElectionId::new(rsnano_types::QualifiedRoot::new_test_instance(), 0),
            winner: BlockHash::from(2),
            vote_type,
        }
    }

    fn t(secs: u64) -> Timestamp {
        Timestamp::new_test_instance() + Duration::from_secs(secs)
    }
}
