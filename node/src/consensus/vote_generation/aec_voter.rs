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

    fn flush(&mut self, queue: &mut Vec<VoteTarget>, now: rsnano_nullable_clock::Timestamp) {
        // TODO: enqueue with one call
        for target in queue.drain(..) {
            if self.vote_generators.generate_vote_with_context(
                &target.root.root,
                &target.winner,
                target.vote_type,
                #[cfg(feature = "rai_protocol")]
                target.metadata.clone(),
                #[cfg(feature = "rai_protocol")]
                target.is_rai_close,
            ) {
                self.scheduler.mark_voted(&target, now);
            }
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
        let now = self.clock.now();
        let scheduler = &self.scheduler;

        // Collect all vote targets in a single lock acquisition, iterating all
        // elections in round-robin order across buckets
        let targets: Vec<VoteTarget> = self.aec.round_robin(|iter| {
            iter.filter_map(|e| {
                let target = vote_target(e);
                #[cfg(feature = "rai_protocol")]
                let target = {
                    let mut target = target;
                    self.vote_generators
                        .ensure_local_first_vote(&target.root.root, &mut target.metadata);
                    target
                };
                if scheduler.can_vote(&target, now) {
                    Some(target)
                } else {
                    None
                }
            })
            .collect()
        });

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

            vote_queue.push(target);

            if cancel_token.is_cancelled() {
                self.flush(&mut vote_queue, now);
                return;
            }
        }

        self.scheduler.cleanup(now);
        self.flush(&mut vote_queue, now);
    }
}
