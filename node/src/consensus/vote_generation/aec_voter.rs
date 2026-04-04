use std::{sync::Arc, time::Duration};

use rsnano_nullable_clock::SteadyClock;
use rsnano_types::NetworkType;
use rsnano_utils::{CancellationToken, ticker::Tickable};

use super::{
    CpsLimiter, VoteGenerators,
    voting_scheduler::{VoteTarget, VotingScheduler},
};
use crate::consensus::{
    AecService,
    election::VoteType,
    election_schedulers::priority::bucket_count,
    vote_generation::voting_scheduler::{get_vote_target, vote_target},
};

/// Creates votes for blocks within the AEC
pub(crate) struct AecVoter {
    aec: Arc<AecService>,
    vote_generators: Arc<VoteGenerators>,
    clock: Arc<SteadyClock>,
    cps_limiter: CpsLimiter,
    current_bucket: usize,
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
            current_bucket: bucket_count() - 1,
            scheduler: VotingScheduler::new(vote_broadcast_interval),
        }
    }

    fn flush(&self, queue: &mut Vec<VoteTarget>) {
        // TODO: enqueue with one call
        for target in queue.drain(..) {
            self.vote_generators
                .generate_vote(&target.root.root, &target.winner, target.vote_type);
        }
    }
}

impl Tickable for AecVoter {
    fn tick(&mut self, cancel_token: &CancellationToken) {
        let now = self.clock.now();
        let scheduler = &self.scheduler;

        // Collect all vote targets in a single lock acquisition
        let targets: Vec<(usize, VoteTarget)> = self.aec.with_elections_starting_from_bucket(
            self.current_bucket,
            |e| scheduler.can_vote(&vote_target(e), now),
            |iter| iter.map(|(bucket, e)| (bucket, vote_target(e))).collect(),
        );

        let mut vote_queue = Vec::new();
        for (bucket, target) in targets {
            if target.vote_type == VoteType::NonFinal && !self.cps_limiter.try_vote(now) {
                self.current_bucket = bucket;
                self.flush(&mut vote_queue);
                return;
            }

            self.current_bucket = if bucket == 0 {
                bucket_count() - 1
            } else {
                bucket - 1
            };

            self.scheduler.mark_voted(&target, now);
            vote_queue.push(target);

            if cancel_token.is_cancelled() {
                self.flush(&mut vote_queue);
                return;
            }
        }

        self.current_bucket = bucket_count() - 1;
        self.scheduler.cleanup(now);
        self.flush(&mut vote_queue);
    }
}
