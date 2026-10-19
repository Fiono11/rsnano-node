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
    /// RAI evaluation, a Byzantine representative voting for every block of
    /// a fork (`ActiveElectionsConfig::byzantine_equivocate`)
    #[cfg(feature = "rai_byzantine")]
    equivocations: Option<super::equivocations::Equivocations>,
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
            #[cfg(feature = "rai_byzantine")]
            equivocations: None,
        }
    }

    /// RAI evaluation: this representative votes for every block of a fork
    #[cfg(feature = "rai_byzantine")]
    pub(crate) fn with_equivocation(mut self, equivocate: bool) -> Self {
        self.equivocations = equivocate.then(Default::default);
        self
    }

    /// RAI evaluation, an equivocating representative: the votes just
    /// released are watched, and every fork sibling of their blocks gets the
    /// same vote, signed now
    #[cfg(feature = "rai_byzantine")]
    fn equivocate(&mut self, released: &[VoteTarget], now: rsnano_nullable_clock::Timestamp) {
        let Some(equivocations) = self.equivocations.as_mut() else {
            return;
        };
        for target in released {
            equivocations.voted(now, target.clone());
        }
        let aec = &self.aec;
        let due = equivocations.due(now, |root| aec.candidate_hashes(root));
        for (target, sibling) in &due {
            self.vote_generators
                .sign_now(&target.root.root, sibling, target.vote_type);
        }
        if !due.is_empty() {
            eprintln!("BYZ_EQUIVOCATED votes={}", due.len());
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

            self.scheduler.mark_voted(&target, now);
            vote_queue.push(target);

            if cancel_token.is_cancelled() {
                self.flush(&mut vote_queue);
                return;
            }
        }

        self.scheduler.cleanup(now);
        #[cfg(feature = "rai_byzantine")]
        let released = vote_queue.clone();
        self.flush(&mut vote_queue);
        #[cfg(feature = "rai_byzantine")]
        self.equivocate(&released, now);
    }
}
