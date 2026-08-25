use std::{
    collections::{HashMap, VecDeque},
    mem::size_of,
    time::Duration,
};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, QualifiedRoot};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

use crate::consensus::election::{Election, VoteType};

pub(crate) struct VoteTarget {
    pub root: QualifiedRoot,
    pub winner: BlockHash,
    pub vote_type: VoteType,
}

#[cfg(not(feature = "rai_protocol"))]
pub(crate) fn vote_target(e: &Election) -> VoteTarget {
    VoteTarget {
        root: e.qualified_root().clone(),
        winner: e.winner().hash(),
        vote_type: e.vote_type(),
    }
}

#[cfg(feature = "rai_protocol")]
pub(crate) fn vote_targets(e: &Election) -> Vec<VoteTarget> {
    if !e.vote_generation_enabled() {
        return Vec::new();
    }
    let mut targets = vec![VoteTarget {
        root: e.qualified_root().clone(),
        winner: e.winner().hash(),
        vote_type: VoteType::First,
    }];
    for winner in e.second_look_targets() {
        targets.push(VoteTarget {
            root: e.qualified_root().clone(),
            winner,
            vote_type: VoteType::NonFinal,
        });
    }
    if e.has_quorum() {
        targets.push(VoteTarget {
            root: e.qualified_root().clone(),
            winner: e.winner().hash(),
            vote_type: VoteType::Final,
        });
    }
    // Once final is eligible, prefer it and do not concurrently queue a timeout.
    // Per-representative history below the generators supplies the durable-for-
    // process-lifetime lock when a timeout was signed on an earlier pass.
    if !e.has_quorum() && e.should_vote_timeout() {
        targets.push(VoteTarget {
            root: e.qualified_root().clone(),
            winner: e.winner().hash(),
            vote_type: VoteType::Timeout,
        });
    }
    targets
}

pub(crate) struct VotingScheduler {
    records: HashMap<QualifiedRoot, VoteRecord>,
    expiry_queue: VecDeque<(Timestamp, QualifiedRoot)>,
    interval: Duration,
}

struct VoteRecord {
    last_non_final: Option<Timestamp>,
    last_final: Option<Timestamp>,
    #[cfg(feature = "rai_protocol")]
    last_first: Option<Timestamp>,
    #[cfg(feature = "rai_protocol")]
    last_timeout: Option<Timestamp>,
    last_voted_winner: BlockHash,
    last_voted: Timestamp,
}

impl VotingScheduler {
    pub fn new(interval: Duration) -> Self {
        Self {
            records: HashMap::new(),
            expiry_queue: VecDeque::new(),
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
            #[cfg(feature = "rai_protocol")]
            VoteType::First => record.last_first,
            #[cfg(feature = "rai_protocol")]
            VoteType::Timeout => record.last_timeout,
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
                #[cfg(feature = "rai_protocol")]
                last_first: None,
                #[cfg(feature = "rai_protocol")]
                last_timeout: None,
                last_voted_winner: BlockHash::ZERO,
                last_voted: now,
            });

        debug_assert!(now >= record.last_voted);

        match target.vote_type {
            VoteType::NonFinal => record.last_non_final = Some(now),
            VoteType::Final => record.last_final = Some(now),
            #[cfg(feature = "rai_protocol")]
            VoteType::First => record.last_first = Some(now),
            #[cfg(feature = "rai_protocol")]
            VoteType::Timeout => record.last_timeout = Some(now),
        }
        record.last_voted_winner = target.winner;
        record.last_voted = now;

        self.expiry_queue.push_back((now, target.root.clone()));
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
                size_of::<(Timestamp, QualifiedRoot)>(),
            ),
        ]
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::representatives::QuorumSnapshot;
    use rsnano_ledger::RepWeights;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{PrivateKey, QualifiedRoot, SavedBlock, Vote};
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

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn eligible_final_vote_excludes_timeout_vote() {
        let block = SavedBlock::new_test_instance();
        let hash = block.hash();
        let mut election = Election::new_test_instance_with(block);
        let quorum = QuorumSnapshot::new_test_instance();
        let certificate = quorum.total_weight - quorum.faulty_weight - quorum.slack_weight;
        let rep = PrivateKey::from(1);
        let mut weights = RepWeights::default();
        weights.put(rep.public_key(), certificate);
        election
            .apply_rai_vote(
                &Vote::new_rai(&rep, 1, VoteType::First, vec![hash]),
                hash,
                Timestamp::new_test_instance(),
            )
            .unwrap();
        election.update_rai_tallies(&weights, &quorum);
        election
            .transition_time(election.start() + Duration::from_mins(5) + Duration::from_millis(1));

        let targets = vote_targets(&election);
        assert!(
            targets
                .iter()
                .any(|target| target.vote_type == VoteType::Final)
        );
        assert!(
            !targets
                .iter()
                .any(|target| target.vote_type == VoteType::Timeout)
        );
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn suppressed_election_has_no_local_vote_targets() {
        let mut election = Election::new_test_instance_with(SavedBlock::new_test_instance());
        election.suppress_vote_generation();

        assert!(vote_targets(&election).is_empty());
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
