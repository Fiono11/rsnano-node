use std::{
    cmp::min,
    collections::{BTreeMap, HashMap, HashSet},
    mem::size_of,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use rsnano_ledger::{AnySet, Ledger, LedgerSet};
use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Amount, BlockHash};
use rsnano_utils::{
    container_info::ContainerInfo,
    stats::{DetailType, StatType, Stats},
};

use super::VoteCache;
use crate::{
    cementation::ConfirmingSet,
    consensus::{AecInsertRequest, AecService, election::ElectionBehavior},
    representatives::RepresentativeTracker,
};

#[derive(Clone, Debug, PartialEq)]
pub struct HintedSchedulerConfig {
    pub check_interval: Duration,
    pub block_cooldown: Duration,
    pub hinting_threshold_percent: u32,
    pub vacancy_threshold_percent: u32,
    /// Limit of hinted elections as percentage of `active_elections_size`
    pub hinted_limit_percentage: usize,
}

impl HintedSchedulerConfig {
    pub fn default_for_dev_network() -> Self {
        Self {
            check_interval: Duration::from_millis(100),
            block_cooldown: Duration::from_millis(100),
            ..Default::default()
        }
    }
}

impl Default for HintedSchedulerConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_millis(1000),
            block_cooldown: Duration::from_millis(5000),
            hinting_threshold_percent: 10,
            vacancy_threshold_percent: 20,
            hinted_limit_percentage: 20,
        }
    }
}

/// Monitors inactive vote cache and schedules elections with the highest observed vote tally.
pub struct HintedScheduler {
    thread: Mutex<Option<JoinHandle<()>>>,
    config: HintedSchedulerConfig,
    active_elections: Arc<AecService>,
    condition: Condvar,
    ledger: Arc<Ledger>,
    confirming_set: Arc<ConfirmingSet>,
    stats: Arc<Stats>,
    vote_cache: Arc<VoteCache>,
    rep_tracker: Arc<RepresentativeTracker>,
    clock: Arc<SteadyClock>,
    stopped: AtomicBool,
    stopped_mutex: Mutex<()>,
    cooldowns: Mutex<OrderedCooldowns>,
    pub max_elections: usize,
}

impl HintedScheduler {
    pub fn new(
        config: HintedSchedulerConfig,
        active_elections: Arc<AecService>,
        ledger: Arc<Ledger>,
        stats: Arc<Stats>,
        vote_cache: Arc<VoteCache>,
        confirming_set: Arc<ConfirmingSet>,
        rep_tracker: Arc<RepresentativeTracker>,
        clock: Arc<SteadyClock>,
    ) -> Self {
        let max_elections = active_elections.max_len() * config.hinted_limit_percentage / 100;

        Self {
            thread: Mutex::new(None),
            config,
            condition: Condvar::new(),
            active_elections,
            ledger,
            stats,
            vote_cache,
            confirming_set,
            rep_tracker,
            clock,
            stopped: AtomicBool::new(false),
            stopped_mutex: Mutex::new(()),
            cooldowns: Mutex::new(OrderedCooldowns::new()),
            max_elections,
        }
    }

    pub fn stop(&self) {
        {
            // Pair the state change with the wait mutex so shutdown cannot miss
            // a notification between the worker's predicate check and its wait.
            let _guard = self.stopped_mutex.lock().unwrap();
            self.stopped.store(true, Ordering::SeqCst);
        }
        self.notify();
        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
    }

    /// The worker checks vacancy on its configured interval. Ordinary vacancy
    /// notifications do not satisfy its wait predicate; only shutdown wakes it.
    pub fn notify(&self) {
        if self.stopped.load(Ordering::SeqCst) {
            self.condition.notify_all();
        }
    }

    fn aec_vacancy(&self) -> i64 {
        let vacancy = self.max_elections as i64
            - self
                .active_elections
                .count_by_behavior(ElectionBehavior::Hinted) as i64;
        min(vacancy, self.active_elections.vacancy())
    }

    pub fn container_info(&self) -> ContainerInfo {
        let guard = self.cooldowns.lock().unwrap();
        [(
            "cooldowns",
            guard.len(),
            (size_of::<BlockHash>() + size_of::<Instant>()) * 2,
        )]
        .into()
    }

    fn predicate(&self) -> bool {
        // Check if there is space inside AEC for a new hinted election
        self.aec_vacancy() > 0
    }

    fn activate(&self, any: &impl AnySet, hash: BlockHash, check_dependents: bool) {
        const MAX_ITERATIONS: usize = 64;
        let mut visited = HashSet::new();
        let mut stack = Vec::new();
        stack.push(hash);
        let mut iterations = 0;
        while let Some(current_hash) = stack.pop() {
            if iterations >= MAX_ITERATIONS {
                break;
            }
            iterations += 1;

            // Check if block exists
            if let Some(block) = any.get_block(&current_hash) {
                let forked = {
                    #[cfg(not(feature = "ledger_snapshots"))]
                    {
                        false
                    }
                    #[cfg(feature = "ledger_snapshots")]
                    {
                        any.is_forked(&block.qualified_root())
                    }
                };

                // Ensure block is not already confirmed
                let is_confirmed = self.confirming_set.contains(&current_hash)
                    || any.confirmed().block_exists(&current_hash);

                if is_confirmed && !forked {
                    self.stats
                        .inc(StatType::Hinting, DetailType::AlreadyConfirmed);
                    self.vote_cache.remove(&current_hash); // Remove from vote cache
                    continue; // Move on to the next item in the stack
                }

                if check_dependents {
                    // Perform a depth-first search of the dependency graph
                    if !any.dependencies_confirmed(&block) {
                        self.stats
                            .inc(StatType::Hinting, DetailType::DependentUnconfirmed);
                        let dependents = any.block_dependencies(&block);
                        for dependent_hash in dependents.iter() {
                            // Avoid visiting the same block twice
                            if !dependent_hash.is_zero() && visited.insert(*dependent_hash) {
                                stack.push(*dependent_hash); // Add dependent block to the stack
                            }
                        }
                        continue; // Move on to the next item in the stack
                    }
                }

                // Try to insert it into AEC as hinted election
                let now = self.clock.now();
                let priority = any.block_priority(&block);
                let inserted = self
                    .active_elections
                    .insert(AecInsertRequest::new_hinted(block, priority), now)
                    .is_ok();

                self.stats.inc(
                    StatType::Hinting,
                    if inserted {
                        DetailType::Insert
                    } else {
                        DetailType::InsertFailed
                    },
                );
            } else {
                self.stats.inc(StatType::Hinting, DetailType::MissingBlock);

                // TODO: Block is missing, bootstrap it
            }
        }
    }

    fn run_interactive(&self) {
        let minimum_tally = self.tally_threshold();
        let minimum_final_tally = self.final_tally_threshold();

        // TODO: reuse buffer
        let mut tops = Vec::new();

        // Get the list before db transaction starts to avoid unnecessary slowdowns
        self.vote_cache.top(&mut tops, minimum_tally);

        let mut any = self.ledger.any();

        for entry in tops {
            if self.stopped.load(Ordering::SeqCst) {
                return;
            }

            if !self.predicate() {
                return;
            }

            if self.cooldown(entry.hash) {
                continue;
            }

            if any.should_refresh() {
                any = self.ledger.any();
            }

            // Check dependents only if cached tally is lower than quorum
            if entry.final_tally < minimum_final_tally {
                // Ensure all dependent blocks are already confirmed before activating
                self.stats.inc(StatType::Hinting, DetailType::Activate);
                self.activate(&any, entry.hash, /* activate dependents */ true);
            } else {
                // Blocks with a vote tally higher than quorum, can be activated and confirmed immediately
                self.stats
                    .inc(StatType::Hinting, DetailType::ActivateImmediate);
                self.activate(&any, entry.hash, false);
            }
        }
    }

    fn run(&self) {
        let mut guard = self.stopped_mutex.lock().unwrap();
        while !self.stopped.load(Ordering::SeqCst) {
            self.stats.inc(StatType::Hinting, DetailType::Loop);
            guard = self
                .condition
                .wait_timeout_while(guard, self.config.check_interval, |_| {
                    !self.stopped.load(Ordering::SeqCst)
                })
                .unwrap()
                .0;
            if !self.stopped.load(Ordering::SeqCst) {
                drop(guard);
                if self.predicate() {
                    self.run_interactive()
                }
                guard = self.stopped_mutex.lock().unwrap();
            }
        }
    }

    fn tally_threshold(&self) -> Amount {
        (self.rep_tracker.quorum_snapshot().trended_or_min_weight / 100)
            * self.config.hinting_threshold_percent as u128
    }

    fn final_tally_threshold(&self) -> Amount {
        self.rep_tracker.quorum_snapshot().quorum_delta
    }

    fn cooldown(&self, hash: BlockHash) -> bool {
        let mut guard = self.cooldowns.lock().unwrap();
        let now = Instant::now();
        // Check if the hash is still in the cooldown period using the hashed index
        if let Some(timeout) = guard.get(&hash) {
            if *timeout > now {
                return true; // Needs cooldown
            }
            guard.remove(&hash); // Entry is outdated, so remove it
        }

        // Insert the new entry
        guard.insert(hash, now + self.config.block_cooldown);

        // Trim old entries
        guard.trim(now);
        false // No need to cooldown
    }
}

impl Drop for HintedScheduler {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        debug_assert!(self.thread.lock().unwrap().is_none());
    }
}

pub trait HintedSchedulerExt {
    fn start(&self);
}

impl HintedSchedulerExt for Arc<HintedScheduler> {
    fn start(&self) {
        debug_assert!(self.thread.lock().unwrap().is_none());
        let self_l = Arc::clone(self);
        *self.thread.lock().unwrap() = Some(
            std::thread::Builder::new()
                .name("Sched Hinted".to_string())
                .spawn(Box::new(move || {
                    self_l.run();
                }))
                .unwrap(),
        );
    }
}

struct OrderedCooldowns {
    by_hash: HashMap<BlockHash, Instant>,
    by_time: BTreeMap<Instant, Vec<BlockHash>>,
}

impl OrderedCooldowns {
    fn new() -> Self {
        Self {
            by_hash: HashMap::new(),
            by_time: BTreeMap::new(),
        }
    }
    fn insert(&mut self, hash: BlockHash, timeout: Instant) {
        if let Some(old_timeout) = self.by_hash.insert(hash, timeout) {
            self.remove_timeout_entry(&hash, old_timeout);
        }
        self.by_time.entry(timeout).or_default().push(hash);
    }

    fn get(&self, hash: &BlockHash) -> Option<&Instant> {
        self.by_hash.get(hash)
    }

    fn remove(&mut self, hash: &BlockHash) {
        if let Some(timeout) = self.by_hash.remove(hash) {
            self.remove_timeout_entry(hash, timeout);
        }
    }

    fn remove_timeout_entry(&mut self, hash: &BlockHash, timeout: Instant) {
        if let Some(hashes) = self.by_time.get_mut(&timeout) {
            if hashes.len() == 1 {
                self.by_time.remove(&timeout);
            } else {
                hashes.retain(|h| h != hash)
            }
        }
    }

    fn trim(&mut self, now: Instant) {
        while let Some(entry) = self.by_time.first_entry() {
            if *entry.key() <= now {
                let hashes = entry.remove();
                for hash in hashes {
                    self.by_hash.remove(&hash);
                }
            } else {
                break;
            }
        }
    }

    fn len(&self) -> usize {
        self.by_hash.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{
        AecCooldownReason, BucketInfo, ElectionCandidate, ElectionCandidateSource,
    };
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::TimePriority;
    use rsnano_utils::stats::Direction;
    use std::sync::mpsc;

    fn scheduler(aec: Arc<AecService>, interval: Duration) -> Arc<HintedScheduler> {
        HintedScheduler::new(
            HintedSchedulerConfig {
                check_interval: interval,
                ..Default::default()
            },
            aec,
            Arc::new(Ledger::new_null()),
            Arc::new(Stats::default()),
            Arc::new(VoteCache::new_null()),
            Arc::new(ConfirmingSet::new_null()),
            Arc::new(RepresentativeTracker::new_null()),
            Arc::new(SteadyClock::new_null()),
        )
        .into()
    }

    struct BlockingCandidates {
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl ElectionCandidateSource for BlockingCandidates {
        fn should_schedule(&self, _buckets: &[BucketInfo]) -> bool {
            true
        }

        fn next_candidate(
            &mut self,
            _bucket_id: usize,
            _vacancy: isize,
            _lowest_priority: TimePriority,
        ) -> Option<ElectionCandidate> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            None
        }
    }

    #[test]
    fn vacancy_notification_does_not_wait_for_aec_writer() {
        let aec = Arc::new(AecService::new_null());
        let scheduler = scheduler(aec.clone(), Duration::from_secs(60));
        let completed = std::thread::scope(|scope| {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let writer = scope.spawn(move || {
                aec.refill(
                    &mut BlockingCandidates {
                        entered: Some(entered_tx),
                        release: release_rx,
                    },
                    Timestamp::new_test_instance(),
                );
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let (completed_tx, completed_rx) = mpsc::channel();
            let notifier = scope.spawn(move || {
                scheduler.notify();
                completed_tx.send(()).unwrap();
            });
            let completed = completed_rx.recv_timeout(Duration::from_secs(2));
            // Release before joining: a regression must fail, rather than hang.
            release_tx.send(()).unwrap();
            writer.join().unwrap();
            notifier.join().unwrap();
            completed
        });
        assert!(
            completed.is_ok(),
            "vacancy notification waited for the AEC writer"
        );
    }

    #[test]
    fn stop_wakes_periodic_wait_even_when_aec_has_no_vacancy() {
        let aec = Arc::new(AecService::new_null());
        aec.set_cooldown(true, AecCooldownReason::AecFactQueueFull);
        let scheduler = scheduler(aec, Duration::from_secs(60));
        assert_eq!(scheduler.aec_vacancy(), 0);
        scheduler.start();
        let deadline = Instant::now() + Duration::from_secs(2);
        while scheduler
            .stats
            .count(StatType::Hinting, DetailType::Loop, Direction::In)
            == 0
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        let started = scheduler
            .stats
            .count(StatType::Hinting, DetailType::Loop, Direction::In)
            > 0;
        // The loop increments its counter under this mutex, immediately before
        // waiting. Acquiring it now ensures the worker has reached its wait.
        drop(scheduler.stopped_mutex.lock().unwrap());
        let completed = std::thread::scope(|scope| {
            let (completed_tx, completed_rx) = mpsc::channel();
            let scheduler_l = scheduler.clone();
            let stopper = scope.spawn(move || {
                scheduler_l.stop();
                completed_tx.send(()).unwrap();
            });
            let completed = completed_rx.recv_timeout(Duration::from_secs(2));
            // Rescue the old vacancy-gated stop path without a 60-second hang.
            scheduler.condition.notify_all();
            stopper.join().unwrap();
            completed
        });
        assert!(started, "scheduler did not enter its periodic wait");
        assert!(
            completed.is_ok(),
            "stop waited for the configured check interval"
        );
    }
}
