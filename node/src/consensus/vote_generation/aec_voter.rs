use std::{sync::Arc, time::Duration};

use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{BlockHash, NetworkType, Root};
use rsnano_utils::{CancellationToken, ticker::Tickable};

use super::{CpsLimiter, VoteGenerators, voting_scheduler::VotingScheduler};
use crate::consensus::{
    AecService, election::VoteType, election_schedulers::priority::bucket_count,
};

/// Creates votes for blocks within the AEC
pub(crate) struct AecVoter {
    aec: Arc<AecService>,
    vote_generators: Arc<VoteGenerators>,
    clock: Arc<SteadyClock>,
    cps_limiter: CpsLimiter,
    current_bucket: usize,
    vote_broadcast_interval: Duration,
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
        Self {
            aec,
            vote_generators,
            clock,
            cps_limiter,
            current_bucket: bucket_count() - 1,
            vote_broadcast_interval: match network {
                NetworkType::NanoDevNetwork => Duration::from_millis(500),
                _ => Duration::from_secs(15),
            },
            scheduler: VotingScheduler::new(),
        }
    }

    fn flush(&self, queue: &mut Vec<(Root, BlockHash, VoteType)>) {
        // TODO: enqueue with one call
        for (root, hash, vote_type) in queue.drain(..) {
            self.vote_generators.generate_vote(&root, &hash, vote_type);
        }
    }
}

impl Tickable for AecVoter {
    fn tick(&mut self, cancel_token: &CancellationToken) {
        let now = self.clock.now();
        let mut vote_queue = Vec::new();

        loop {
            let scheduler = &self.scheduler;
            let interval = self.vote_broadcast_interval;
            let vote_target =
                self.aec
                    .with_elections_starting_from_bucket(self.current_bucket, |elections| {
                        let mut found = None;
                        for (bucket, e) in elections {
                            let root = e.qualified_root();
                            let winner = e.winner().hash();
                            if scheduler.can_vote(root, interval, now, winner, e.vote_type()) {
                                found = Some((bucket, root.clone(), winner, e.vote_type()));
                                break;
                            }
                        }
                        found
                    });

            let Some((bucket, root, winner_hash, vote_type)) = vote_target else {
                self.current_bucket = bucket_count() - 1;
                break;
            };

            if vote_type == VoteType::NonFinal && !self.cps_limiter.try_vote(now) {
                self.current_bucket = bucket;
                self.flush(&mut vote_queue);
                return;
            }

            self.current_bucket = if bucket == 0 {
                bucket_count() - 1
            } else {
                bucket - 1
            };

            vote_queue.push((root.root, winner_hash, vote_type));
            self.scheduler
                .mark_voted(&root, vote_type, now, winner_hash);

            if cancel_token.is_cancelled() {
                self.flush(&mut vote_queue);
                return;
            }
        }

        self.scheduler.cleanup(now, self.vote_broadcast_interval);
        self.flush(&mut vote_queue);
    }
}
