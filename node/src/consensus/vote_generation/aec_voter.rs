use std::{sync::Arc, time::Duration};

use rsnano_ledger::{AnySet, Ledger};
use rsnano_nullable_clock::{SteadyClock, Timestamp};
use rsnano_types::{BlockHash, NetworkType};
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
    ledger: Arc<Ledger>,
}

impl AecVoter {
    pub(crate) fn new(
        aec: Arc<AecService>,
        vote_generators: Arc<VoteGenerators>,
        clock: Arc<SteadyClock>,
        network: NetworkType,
        cps_limiter: CpsLimiter,
        ledger: Arc<Ledger>,
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
            ledger,
        }
    }

    fn flush(&self, queue: &mut Vec<VoteTarget>) {
        // TODO: enqueue with one call
        for target in queue.drain(..) {
            self.vote_generators.generate_vote(
                &target.election.root.root,
                &target.winner,
                target.election.epoch,
                target.vote_type,
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

impl AecVoter {
    /// Collect all vote targets in a single lock acquisition, iterating all
    /// elections in round-robin order across buckets
    fn collect_targets(&self, now: Timestamp) -> Vec<VoteTarget> {
        let scheduler = &self.scheduler;
        if cfg!(feature = "rai_protocol") {
            // RAI: a block is a valid proposal once its dependencies are
            // finalized here (the previous block, the source of a receive)
            let any = self.ledger.any();
            let checkpoint = self.aec.latest_checkpoint();
            let proposal_valid = |hash: &BlockHash| {
                any.get_block(hash).is_some_and(|block| {
                    crate::consensus::dependencies_attachable(&any, &block, checkpoint.as_deref())
                })
            };
            self.aec
                .kudzu_votes_due(proposal_valid)
                .into_iter()
                .filter(|target| scheduler.can_vote(target, now))
                .collect()
        } else {
            self.aec.round_robin(|iter| {
                iter.filter_map(|e| {
                    let target = vote_target(e);
                    if scheduler.can_vote(&target, now) {
                        Some(target)
                    } else {
                        None
                    }
                })
                .collect()
            })
        }
    }
}

impl Tickable for AecVoter {
    fn tick(&mut self, cancel_token: &CancellationToken) {
        let now = self.clock.now();
        let targets = self.collect_targets(now);

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
                break;
            }
        }

        self.scheduler.cleanup(now);
        if cfg!(feature = "rai_protocol") {
            // Record the decisions before the generators pick them up
            vote_queue = self.aec.mark_kudzu_voted(vote_queue);
        }
        self.flush(&mut vote_queue);
    }
}
