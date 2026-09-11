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
        #[cfg(feature = "rai_protocol")]
        let mut targets: Vec<VoteTarget> = self.aec.round_robin(|iter| {
            let mut targets = Vec::new();
            for e in iter {
                if !eligible(e.qualified_root(), e.epoch) || e.is_confirmed() || e.is_timed_out() {
                    continue;
                }
                let primary = vote_target(e);
                let primary_hash = primary.winner;
                let primary_type = primary.vote_type;
                if scheduler.can_vote(&primary, now) {
                    targets.push(primary);
                }
                for hash in e.candidate_blocks().keys() {
                    if (*hash != primary_hash || primary_type != VoteType::NonFinal)
                        && e.can_notarize(hash)
                    {
                        let target = VoteTarget {
                            root: e.id(),
                            winner: *hash,
                            vote_type: VoteType::NonFinal,
                            timeout: e.should_timeout(),
                        };
                        if scheduler.can_vote(&target, now) {
                            targets.push(target);
                        }
                    }
                }
            }
            targets
        });

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
