use std::{sync::Arc, time::Duration};

use rsnano_nullable_clock::SteadyClock;
use rsnano_types::NetworkType;
use rsnano_utils::{
    CancellationToken,
    container_info::{ContainerInfo, ContainerInfoProvider},
    ticker::Tickable,
};

use super::{
    CpsLimiter, VoteGenerators,
    voting_scheduler::{VoteTarget, VotingScheduler},
};
use crate::consensus::{
    AecService, election::VoteType, vote_generation::voting_scheduler::vote_target,
};

/// Creates votes for blocks within the AEC
pub(crate) struct AecVoter {
    aec: Arc<AecService>,
    vote_generators: Arc<VoteGenerators>,
    clock: Arc<SteadyClock>,
    cps_limiter: CpsLimiter,
    scheduler: VotingScheduler,
}

impl AecVoter {
    pub(crate) fn new(
        aec: Arc<AecService>,
        vote_generators: Arc<VoteGenerators>,
        clock: Arc<SteadyClock>,
        network: NetworkType,
        cps_limiter: CpsLimiter,
    ) -> Self {
        let vote_broadcast_interval = match network {
            NetworkType::NanoDevNetwork => Duration::from_millis(500),
            _ => Duration::from_secs(15),
        };
        Self {
            aec,
            vote_generators,
            clock,
            cps_limiter,
            scheduler: VotingScheduler::new(vote_broadcast_interval),
        }
    }

    fn flush(&self, queue: &mut Vec<VoteTarget>) {
        // TODO: enqueue with one call
        for target in queue.drain(..) {
            self.vote_generators.generate_vote_in_epoch(
                &target.root.root.root,
                &target.winner,
                target.vote_type,
                target.root.epoch,
            );
        }
    }
}

impl ContainerInfoProvider for AecVoter {
    fn container_info(&self) -> ContainerInfo {
        ContainerInfo::builder()
            .node("scheduler", self.scheduler.container_info())
            .finish()
    }
}

impl Tickable for AecVoter {
    fn tick(&mut self, cancel_token: &CancellationToken) {
        #[cfg(feature = "rai_protocol")]
        self.vote_generators
            .notify_notarizations(self.aec.take_notarization_notifications());
        let now = self.clock.now();
        let scheduler = &self.scheduler;

        // Collect all vote targets in a single lock acquisition, iterating all
        // elections in round-robin order across buckets
        #[cfg(not(feature = "rai_protocol"))]
        let targets: Vec<VoteTarget> = self.aec.round_robin(|iter| {
            iter.filter_map(|e| {
                let target = vote_target(e);
                if scheduler.can_vote(&target, now) {
                    Some(target)
                } else {
                    None
                }
            })
            .collect()
        });
        #[cfg(feature = "rai_protocol")]
        let eligible = self.vote_generators.solicitation_filter();
        // A cross-notarized fork can never be finalized by this node, so its
        // Final target would only be collected and rejected on every tick.
        #[cfg(feature = "rai_protocol")]
        let frozen = self.vote_generators.frozen_roots();
        #[cfg(feature = "rai_protocol")]
        let mut retired = Vec::new();
        #[cfg(feature = "rai_protocol")]
        let mut targets: Vec<VoteTarget> = self.aec.round_robin(|iter| {
            let mut targets = Vec::new();
            for e in iter {
                if !eligible(e.qualified_root(), e.epoch) || e.is_confirmed() {
                    continue;
                }
                // A notarized value this node cross-notarized cannot get its
                // final vote, and timeout does not apply once notarized: the
                // election has nothing left to collect from this scan.
                if e.is_timed_out() || (e.has_quorum() && frozen.contains(e.qualified_root())) {
                    retired.push(e.id());
                    continue;
                }
                collect_election_targets(e, scheduler, now, &frozen, &mut targets);
            }
            targets
        });
        #[cfg(feature = "rai_protocol")]
        self.aec.retire_unfinalizable(&retired);

        #[cfg(feature = "rai_protocol")]
        self.vote_generators.retain_signable_targets(&mut targets);

        let mut vote_queue = Vec::new();
        let mut skip_non_final = false;
        for target in targets {
            if target.vote_type == VoteType::NonFinal {
                if skip_non_final {
                    continue;
                }
                // we limit non final votes to reduce CPS
                if !self.cps_limiter.try_vote(now) {
                    skip_non_final = true;
                    continue;
                }
            }

            self.scheduler.mark_voted(&target, now);
            vote_queue.push(target);

            if cancel_token.is_cancelled() {
                self.flush(&mut vote_queue);
                return;
            }
        }

        self.scheduler.cleanup(now);
        self.flush(&mut vote_queue);
    }
}

#[cfg(feature = "rai_protocol")]
fn collect_election_targets(
    election: &crate::consensus::election::Election,
    scheduler: &VotingScheduler,
    now: rsnano_nullable_clock::Timestamp,
    frozen: &rustc_hash::FxHashSet<rsnano_types::QualifiedRoot>,
    targets: &mut Vec<VoteTarget>,
) {
    let primary_hash = election.winner().hash();
    let primary_type = election.vote_type();
    // Eligibility is stable while the caller holds the AEC read lock. Compute
    // timeout once even when several fork candidates need non-final votes.
    let timeout = if primary_type != VoteType::Final || !frozen.contains(election.qualified_root())
    {
        let primary = vote_target(election);
        let timeout = primary.timeout;
        if scheduler.can_vote(&primary, now) {
            targets.push(primary);
        }
        timeout
    } else {
        election.should_timeout()
    };
    if primary_type == VoteType::NonFinal && election.block_count() == 1 {
        return;
    }
    for hash in election.candidate_blocks().keys() {
        if (*hash != primary_hash || primary_type != VoteType::NonFinal)
            && election.can_notarize_known_candidate(hash)
        {
            let target = VoteTarget {
                root: election.id(),
                winner: *hash,
                vote_type: VoteType::NonFinal,
                timeout,
            };
            if scheduler.can_vote(&target, now) {
                targets.push(target);
            }
        }
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use crate::consensus::election::Election;
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{Amount, Block, PrivateKey, SavedBlock, StateBlockArgs, Vote, VoteKind};

    #[test]
    fn frozen_final_target_keeps_secondary_notarization_and_retry_timing() {
        let args = StateBlockArgs::new_test_instance();
        let a = SavedBlock::new_test_instance_with(args.clone().into());
        let b: Block = StateBlockArgs {
            representative: 999.into(),
            ..args
        }
        .into();
        let mut election = Election::new_test_instance_with(a.clone());
        election.try_add_fork(&b, Amount::ZERO);
        let now = Timestamp::new_test_instance();
        let keys: Vec<_> = (1..=6).map(PrivateKey::from).collect();
        let weights = keys
            .iter()
            .map(|key| (key.public_key(), Amount::raw(100)))
            .collect();
        for (i, key) in keys.iter().enumerate() {
            let hash = if i < 3 { a.hash() } else { b.hash() };
            election
                .add_kudzu_vote(
                    Arc::new(Vote::new_with_kind(key, vec![hash], 0, VoteKind::First)),
                    hash,
                    now,
                )
                .unwrap();
        }
        election.update_kudzu_tallies(&weights, Amount::raw(600));
        assert!(election.should_timeout());
        let mut scheduler = VotingScheduler::new(Duration::from_millis(500));
        let frozen = [election.qualified_root().clone()].into_iter().collect();
        let mut targets = Vec::new();
        collect_election_targets(&election, &scheduler, now, &frozen, &mut targets);
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().all(|target| target.timeout));
        for target in targets.drain(..) {
            scheduler.mark_voted(&target, now);
        }
        collect_election_targets(
            &election,
            &scheduler,
            now + Duration::from_millis(100),
            &frozen,
            &mut targets,
        );
        assert!(targets.is_empty());
        collect_election_targets(
            &election,
            &scheduler,
            now + Duration::from_millis(500),
            &frozen,
            &mut targets,
        );
        assert_eq!(targets.len(), 2);

        election
            .add_kudzu_vote(
                Arc::new(Vote::new_with_kind(
                    &keys[3],
                    vec![a.hash()],
                    0,
                    VoteKind::Notarize,
                )),
                a.hash(),
                now,
            )
            .unwrap();
        election.update_kudzu_tallies(&weights, Amount::raw(600));
        assert!(election.has_quorum());
        assert!(!election.is_confirmed());
        targets.clear();
        collect_election_targets(&election, &scheduler, now, &frozen, &mut targets);
        assert_eq!(targets.len(), 2);
        assert!(
            targets
                .iter()
                .all(|target| target.vote_type == VoteType::NonFinal && !target.timeout)
        );
        assert!(targets.iter().any(|target| target.winner == a.hash()));
        assert!(targets.iter().any(|target| target.winner == b.hash()));
    }
}
