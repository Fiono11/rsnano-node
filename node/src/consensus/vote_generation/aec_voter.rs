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

fn due_vote_targets(
    aec: &AecService,
    scheduler: &VotingScheduler,
    now: rsnano_nullable_clock::Timestamp,
    mut prepare: impl FnMut(&mut VoteTarget),
    mut try_non_final: impl FnMut() -> bool,
) -> Vec<VoteTarget> {
    // Snapshot value-only targets while holding the AEC read lock. Any target
    // preparation may acquire locks whose owners query the AEC, so it must run
    // only after this guard has been released.
    let mut skip_non_final = false;
    aec.round_robin(|iter| iter.map(vote_target).collect::<Vec<_>>())
        .into_iter()
        .filter_map(|mut target| {
            if target.vote_type == VoteType::NonFinal && skip_non_final {
                return None;
            }
            #[cfg(feature = "rai_protocol")]
            if !scheduler.may_vote_after_rai_preparation(&target, now) {
                return None;
            }
            #[cfg(not(feature = "rai_protocol"))]
            if !scheduler.can_vote(&target, now) {
                return None;
            }
            prepare(&mut target);
            if !scheduler.can_vote(&target, now) {
                return None;
            }
            if target.vote_type == VoteType::NonFinal && !try_non_final() {
                // The limiter is shared by every non-final election. Once it
                // rejects, skip target preparation for the rest of this pass.
                skip_non_final = true;
                return None;
            }
            Some(target)
        })
        .collect()
}

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

        let targets = due_vote_targets(
            &self.aec,
            scheduler,
            now,
            |target| {
                #[cfg(feature = "rai_protocol")]
                self.vote_generators
                    .ensure_local_first_vote(&target.root.root, &mut target.metadata);
                #[cfg(not(feature = "rai_protocol"))]
                let _ = target;
            },
            || self.cps_limiter.try_vote(now),
        );

        let mut vote_queue = Vec::new();
        for target in targets {
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

#[cfg(test)]
mod tests {
    use std::{cell::Cell, time::Duration};

    use rsnano_types::{BlockPriority, SavedBlock};

    use super::*;
    use crate::consensus::AecInsertRequest;

    #[test]
    fn releases_aec_before_preparing_vote_targets() {
        let aec = AecService::new_null();
        let now = rsnano_nullable_clock::Timestamp::new_test_instance();
        aec.insert(
            AecInsertRequest::new_manual(
                SavedBlock::new_test_instance(),
                BlockPriority::new_test_instance(),
            ),
            now,
        )
        .unwrap();
        let scheduler = VotingScheduler::new(Duration::from_secs(1));
        let prepared = Cell::new(false);

        let targets = due_vote_targets(
            &aec,
            &scheduler,
            now,
            |_| {
                prepared.set(true);
                assert!(aec.write_lock_available_for_test());
            },
            || true,
        );

        assert!(prepared.get());
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn cooling_target_is_filtered_before_preparation() {
        let aec = AecService::new_null();
        let now = rsnano_nullable_clock::Timestamp::new_test_instance();
        aec.insert(
            AecInsertRequest::new_manual(
                SavedBlock::new_test_instance(),
                BlockPriority::new_test_instance(),
            ),
            now,
        )
        .unwrap();
        let mut scheduler = VotingScheduler::new(Duration::from_secs(1));
        let target = aec.round_robin(|iter| vote_target(iter.next().unwrap()));
        scheduler.mark_voted(&target, now);
        let prepared = Cell::new(false);

        let targets = due_vote_targets(&aec, &scheduler, now, |_| prepared.set(true), || true);

        assert!(!prepared.get());
        assert!(targets.is_empty());
    }

    #[test]
    fn limiter_rejection_skips_preparing_later_non_final_targets() {
        let aec = AecService::new_null();
        let now = rsnano_nullable_clock::Timestamp::new_test_instance();
        for key in [1, 2] {
            aec.insert(
                AecInsertRequest::new_manual(
                    SavedBlock::new_test_instance_with_key(key),
                    BlockPriority::new_test_instance(),
                ),
                now,
            )
            .unwrap();
        }
        let scheduler = VotingScheduler::new(Duration::from_secs(1));
        let prepared = Cell::new(0);
        let limiter_checks = Cell::new(0);

        let targets = due_vote_targets(
            &aec,
            &scheduler,
            now,
            |_| prepared.set(prepared.get() + 1),
            || {
                limiter_checks.set(limiter_checks.get() + 1);
                false
            },
        );

        assert_eq!(prepared.get(), 1);
        assert_eq!(limiter_checks.get(), 1);
        assert!(targets.is_empty());
    }
}
