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
    AecService, SigningRecords, vote_generation::voting_scheduler::vote_target,
};

/// Creates votes for blocks within the AEC
pub(crate) struct AecVoter {
    aec: Arc<AecService>,
    vote_generators: Arc<VoteGenerators>,
    clock: Arc<SteadyClock>,
    cps_limiter: CpsLimiter,
    scheduler: VotingScheduler,
    ledger: Arc<Ledger>,
    /// RAI: the signing records persisted before votes are released
    signing: Arc<SigningRecords>,
    /// RAI evaluation, an equivocating Byzantine member: its votes on
    /// account slots, whose fork siblings it votes for too
    #[cfg(feature = "rai_byzantine")]
    equivocations: crate::consensus::byzantine::Equivocations,
}

impl AecVoter {
    pub(crate) fn new(
        aec: Arc<AecService>,
        vote_generators: Arc<VoteGenerators>,
        clock: Arc<SteadyClock>,
        network: NetworkType,
        cps_limiter: CpsLimiter,
        ledger: Arc<Ledger>,
        signing: Arc<SigningRecords>,
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
            signing,
            #[cfg(feature = "rai_byzantine")]
            equivocations: Default::default(),
        }
    }

    /// RAI evaluation, an equivocating Byzantine member: the first and final
    /// votes just released on account slots are watched, and every fork
    /// sibling of their blocks gets the same vote, signed now
    #[cfg(feature = "rai_byzantine")]
    fn equivocate(&mut self, released: &[VoteTarget], now: Timestamp) {
        use crate::consensus::{byzantine::VoteAttack, election::VoteType};
        if self.aec.byzantine().votes != VoteAttack::Equivocate {
            return;
        }
        for target in released {
            if !target.election.epoch.is_close_round()
                && matches!(target.vote_type, VoteType::NonFinal | VoteType::Final)
            {
                self.equivocations.voted(now, target.clone());
            }
        }
        let aec = &self.aec;
        let due = self
            .equivocations
            .due(now, |election| aec.candidate_hashes(election));
        for (target, sibling) in &due {
            self.vote_generators.sign_now(
                &target.election.root.root,
                sibling,
                target.election.epoch,
                target.vote_type,
            );
        }
        if !due.is_empty() {
            crate::utils::diagnostic!("BYZ_EQUIVOCATED votes={}", due.len());
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
        {
            #[cfg(feature = "rai_protocol")]
            {
                // RAI, "Attachment and eligibility": a block is a valid
                // proposal on a finalized parent or a maximum-depth tip of
                // the latest checkpoint, with a finalized receive source;
                // the container lets a complete current-epoch parent stand
                // in for the previous block
                let any = self.ledger.any();
                let checkpoint = self.aec.latest_checkpoint();
                let proposal_valid = |hash: &BlockHash| match any.get_block(hash) {
                    Some(block) => match crate::consensus::unattached_dependency(
                        &any,
                        &block,
                        checkpoint.as_deref(),
                    ) {
                        None => Ok(()),
                        Some(why) => Err(why),
                    },
                    None => Err(crate::consensus::Unattached::Block),
                };
                self.aec
                    .kudzu_votes_due(proposal_valid)
                    .into_iter()
                    .filter(|target| scheduler.can_vote(target, now))
                    .collect()
            }
            #[cfg(not(feature = "rai_protocol"))]
            {
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
}

impl Tickable for AecVoter {
    fn tick(&mut self, cancel_token: &CancellationToken) {
        let now = self.clock.now();
        let targets = self.collect_targets(now);

        let mut vote_queue = Vec::new();
        let mut skip_non_final = false;
        for target in targets {
            if target.vote_type.is_first() {
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
        #[cfg(feature = "rai_protocol")]
        {
            // Record the decisions before the generators pick them up, and
            // persist them before the signatures leave: one write per batch
            vote_queue = self.aec.mark_kudzu_voted(vote_queue);
            let (slots, closes) = self.aec.take_signing_batch();
            self.signing.write_signing(&slots, &closes);
        }
        #[cfg(feature = "rai_byzantine")]
        let released = vote_queue.clone();
        self.flush(&mut vote_queue);
        #[cfg(feature = "rai_byzantine")]
        self.equivocate(&released, now);
    }
}
