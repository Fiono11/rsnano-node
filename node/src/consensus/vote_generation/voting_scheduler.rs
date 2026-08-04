use std::{
    collections::{HashMap, VecDeque},
    mem::size_of,
    time::Duration,
};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{BlockHash, QualifiedRoot};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

use crate::consensus::election::{Election, VoteType};

#[cfg(feature = "rai_protocol")]
type VoteTargetId = crate::consensus::election::RaiElectionId;
#[cfg(not(feature = "rai_protocol"))]
type VoteTargetId = QualifiedRoot;

pub(crate) struct VoteTarget {
    id: VoteTargetId,
    pub root: QualifiedRoot,
    pub winner: BlockHash,
    pub vote_type: VoteType,
    #[cfg(feature = "rai_protocol")]
    pub metadata: rsnano_types::RaiVoteMetadata,
    #[cfg(feature = "rai_protocol")]
    pub is_rai_close: bool,
}

pub(crate) fn vote_target(e: &Election) -> VoteTarget {
    VoteTarget {
        #[cfg(feature = "rai_protocol")]
        id: e.rai_id().clone(),
        #[cfg(not(feature = "rai_protocol"))]
        id: e.qualified_root().clone(),
        root: e.qualified_root().clone(),
        winner: {
            #[cfg(feature = "rai_protocol")]
            {
                e.voting_hash()
            }
            #[cfg(not(feature = "rai_protocol"))]
            {
                e.winner().hash()
            }
        },
        vote_type: e.vote_type(),
        #[cfg(feature = "rai_protocol")]
        metadata: e.rai_vote_metadata(),
        #[cfg(feature = "rai_protocol")]
        is_rai_close: e.is_rai_close(),
    }
}

pub(crate) struct VotingScheduler {
    records: HashMap<VoteTargetId, VoteRecord>,
    expiry_queue: VecDeque<(Timestamp, VoteTargetId)>,
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
            records: HashMap::new(),
            expiry_queue: VecDeque::new(),
            interval,
        }
    }

    /// Returns true if enough time has passed since the last vote for this election,
    /// or if the winner has changed since the last vote.
    pub fn can_vote(&self, target: &VoteTarget, now: Timestamp) -> bool {
        let Some(record) = self.records.get(&target.id) else {
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
        let record = self.records.entry(target.id.clone()).or_insert(VoteRecord {
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

        self.expiry_queue.push_back((now, target.id.clone()));
    }

    /// Remove entries whose most recent vote is older than the interval.
    /// Called once per tick to bound memory usage.
    pub fn cleanup(&mut self, now: Timestamp) {
        while let Some(&(ts, ref id)) = self.expiry_queue.front() {
            if now < ts + self.interval {
                break;
            }
            let id = id.clone();
            self.expiry_queue.pop_front();
            if let Some(record) = self.records.get(&id) {
                if record.last_voted == ts {
                    self.records.remove(&id);
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
                size_of::<(Timestamp, VoteTargetId)>(),
            ),
        ]
        .into()
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

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn vote_target_carries_the_election_context() {
        use crate::consensus::election::ElectionBehavior;
        use rsnano_types::{RaiEpoch, SavedBlock};

        let governing_hash = BlockHash::from(42);
        let election = Election::new_slot(
            SavedBlock::new_test_instance(),
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            t(0),
            RaiEpoch::new(3),
        )
        .with_rai_governing_hash(Some(governing_hash));

        let target = vote_target(&election);

        assert_eq!(target.metadata.epoch, RaiEpoch::new(3));
        assert_eq!(target.metadata.governing_hash, governing_hash);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn close_vote_targets_the_digest_not_the_placeholder_block() {
        use crate::consensus::rai::{RaiCloseElectionId, RaiCloseKind};
        use rsnano_ledger::RepWeights;
        use rsnano_types::RaiEpoch;
        use std::sync::Arc;

        let candidate = BlockHash::from(42);
        let election = Election::new_close(
            RaiCloseElectionId {
                kind: RaiCloseKind::Cut,
                epoch: RaiEpoch::ZERO,
                round: 0,
            },
            QualifiedRoot::new_test_instance(),
            candidate,
            Arc::new(RepWeights::default()),
            Duration::from_secs(1),
            t(0),
        );

        assert_eq!(vote_target(&election).winner, candidate);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn old_epoch_cooldown_does_not_suppress_new_epoch() {
        use crate::consensus::election::{ElectionBehavior, RaiElectionId, RaiSlotId};
        use rsnano_types::{RaiEpoch, SavedBlock};

        let block = SavedBlock::new_test_instance();
        let root = block.qualified_root();
        let old = Election::new_slot(
            block.clone(),
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            t(0),
            RaiEpoch::ZERO,
        );
        let new = Election::new_slot(
            block,
            ElectionBehavior::Priority,
            Duration::from_secs(1),
            t(0),
            RaiEpoch::new(1),
        );
        assert_eq!(
            old.rai_id(),
            &RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::ZERO,
                root
            })
        );

        let mut scheduler = scheduler();
        scheduler.mark_voted(&vote_target(&old), t(0));

        assert!(scheduler.can_vote(&vote_target(&new), t(1)));
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
            #[cfg(feature = "rai_protocol")]
            id: crate::consensus::election::RaiElectionId::Slot(
                crate::consensus::election::RaiSlotId {
                    epoch: Default::default(),
                    root: QualifiedRoot::new_test_instance(),
                },
            ),
            #[cfg(not(feature = "rai_protocol"))]
            id: QualifiedRoot::new_test_instance(),
            root: QualifiedRoot::new_test_instance(),
            winner: BlockHash::from(1),
            vote_type,
            #[cfg(feature = "rai_protocol")]
            metadata: Default::default(),
            #[cfg(feature = "rai_protocol")]
            is_rai_close: false,
        }
    }

    fn other_winner_target(vote_type: VoteType) -> VoteTarget {
        VoteTarget {
            #[cfg(feature = "rai_protocol")]
            id: crate::consensus::election::RaiElectionId::Slot(
                crate::consensus::election::RaiSlotId {
                    epoch: Default::default(),
                    root: QualifiedRoot::new_test_instance(),
                },
            ),
            #[cfg(not(feature = "rai_protocol"))]
            id: QualifiedRoot::new_test_instance(),
            root: QualifiedRoot::new_test_instance(),
            winner: BlockHash::from(2),
            vote_type,
            #[cfg(feature = "rai_protocol")]
            metadata: Default::default(),
            #[cfg(feature = "rai_protocol")]
            is_rai_close: false,
        }
    }

    fn t(secs: u64) -> Timestamp {
        Timestamp::new_test_instance() + Duration::from_secs(secs)
    }
}
