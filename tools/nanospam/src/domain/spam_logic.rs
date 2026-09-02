use crate::domain::{
    AccountMap, BlockFactory, BlockResult, DelayedBlocks, Forks, RateSpec, SpamStrategy,
    high_prio_tracker::HighPrioTracker,
};
use rsnano_network::token_bucket::TokenBucketLogic;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Amount, Blake2Hash, Block, BlockHash};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

pub(crate) struct SpamSpec {
    pub(crate) spam_strategy: SpamStrategy,
    pub(crate) max_blocks: usize,
    pub(crate) rate: RateSpec,
    pub(crate) fork_probability: f64,
    pub(crate) track_confirmations: bool,
    #[cfg(feature = "rai_protocol")]
    pub(crate) expected_epochs: usize,
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let rank = (sorted.len() * percentile).div_ceil(100);
    sorted[rank.saturating_sub(1)]
}

pub(crate) struct SpamLogic {
    pub(crate) delayed: DelayedBlocks,
    pub(crate) high_prio_tracker: HighPrioTracker,
    pub(crate) block_factory: BlockFactory,
    pub(crate) current_bps: usize,
    bps_limiter: TokenBucketLogic,
    next_block: Option<Forks>,
    bps_start: Option<Timestamp>,
    spec: SpamSpec,
    pub(crate) confirmed_total: usize,
    pub(crate) confirmed_recent: usize,
    pub(crate) sum_conf_time_recent: Duration,
    pub(crate) sum_conf_time_total: Duration,
    pub(crate) terminated_total: usize,
    #[cfg(feature = "rai_protocol")]
    pub(crate) fast_finalized_total: usize,
    #[cfg(feature = "rai_protocol")]
    pub(crate) final_finalized_total: usize,
    pub(crate) sum_termination_time_total: Duration,
    terminated: HashSet<BlockHash>,
    published: HashSet<BlockHash>,
    first_publish_at: Option<Timestamp>,
    last_publish_at: Option<Timestamp>,
    pub(crate) cps_measure_start: Option<Timestamp>,
    #[cfg(feature = "rai_protocol")]
    cuts_by_epoch: HashMap<u64, EpochCutState>,
    #[cfg(feature = "rai_protocol")]
    cut_reports: HashMap<u64, HashMap<usize, (Blake2Hash, Timestamp)>>,
    #[cfg(feature = "rai_protocol")]
    finalization_times: HashMap<BlockHash, Duration>,
    #[cfg(feature = "rai_protocol")]
    finalized_at: HashMap<BlockHash, Timestamp>,
    #[cfg(feature = "rai_protocol")]
    epochs_completed: usize,
    #[cfg(feature = "rai_protocol")]
    completed_epoch_ids: HashSet<u64>,
    #[cfg(feature = "rai_protocol")]
    epoch_reports: HashMap<u64, HashMap<usize, (u64, String, u32, Timestamp)>>,
    #[cfg(feature = "rai_protocol")]
    epoch_error: Option<String>,
}

impl SpamLogic {
    pub(crate) fn new(account_map: AccountMap, spec: SpamSpec) -> Self {
        Self {
            delayed: Default::default(),
            high_prio_tracker: Default::default(),
            block_factory: BlockFactory::new(account_map, spec.max_blocks, spec.spam_strategy),
            current_bps: spec.rate.initial_bps,
            bps_limiter: TokenBucketLogic::new(spec.rate.initial_bps),
            next_block: None,
            bps_start: None,
            spec,
            confirmed_total: 0,
            confirmed_recent: 0,
            sum_conf_time_recent: Duration::ZERO,
            sum_conf_time_total: Duration::ZERO,
            terminated_total: 0,
            #[cfg(feature = "rai_protocol")]
            fast_finalized_total: 0,
            #[cfg(feature = "rai_protocol")]
            final_finalized_total: 0,
            sum_termination_time_total: Duration::ZERO,
            terminated: HashSet::new(),
            published: HashSet::new(),
            first_publish_at: None,
            last_publish_at: None,
            cps_measure_start: None,
            #[cfg(feature = "rai_protocol")]
            cuts_by_epoch: Default::default(),
            #[cfg(feature = "rai_protocol")]
            cut_reports: Default::default(),
            #[cfg(feature = "rai_protocol")]
            finalization_times: Default::default(),
            #[cfg(feature = "rai_protocol")]
            finalized_at: Default::default(),
            #[cfg(feature = "rai_protocol")]
            epochs_completed: 0,
            #[cfg(feature = "rai_protocol")]
            completed_epoch_ids: Default::default(),
            #[cfg(feature = "rai_protocol")]
            epoch_reports: Default::default(),
            #[cfg(feature = "rai_protocol")]
            epoch_error: None,
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        let max_blocks = self.block_factory.max_blocks();
        #[cfg(feature = "rai_protocol")]
        {
            // Termination/notarization releases dependent block generation, but it is not
            // finality. Keep the run alive until every requested block has actually been
            // finalized (or the outer timeout cancels it).
            if self.spec.expected_epochs > 0 {
                self.epochs_completed >= self.spec.expected_epochs
            } else {
                max_blocks > 0 && self.confirmed_total >= max_blocks
            }
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            max_blocks > 0 && self.confirmed_total >= max_blocks
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn terminated(&mut self, hash: &BlockHash, timeout: bool, now: Timestamp) -> bool {
        let Some(primary) = self.delayed.primary_hash(hash) else {
            return false;
        };
        if !self.record_termination(primary, now) {
            return false;
        }
        if timeout {
            self.block_factory.rollback(&primary);
            self.delayed.discard(&primary);
        } else {
            self.block_factory.terminate(hash);
        }
        true
    }

    #[cfg(feature = "rai_protocol")]
    fn record_termination(&mut self, primary: BlockHash, now: Timestamp) -> bool {
        if !self.terminated.insert(primary) {
            return false;
        }
        self.terminated_total += 1;
        if let Some(elapsed) = self.delayed.elapsed_since_first_publish(&primary, now) {
            self.sum_termination_time_total += elapsed;
        }
        true
    }

    pub(crate) fn fork_propability(&self) -> f64 {
        self.spec.fork_probability
    }

    pub(crate) fn next_block(&mut self, is_fork: bool, now: Timestamp) -> Option<BlockResult> {
        // A block may already have been built before the rate limiter admitted it. Drain that
        // pending block even though building it incremented the factory's created count to the
        // configured maximum.
        if self.next_block.is_none() && self.block_factory.max_blocks_reached() {
            return None;
        }

        if self.bps_start.is_none() {
            self.bps_start = Some(now);
        }

        if self.next_block.is_none() {
            match self.block_factory.create_next(is_fork) {
                Some(BlockResult::Block(b)) => {
                    self.next_block = Some(b);
                }
                Some(BlockResult::Waiting) => return Some(BlockResult::Waiting),
                None => unreachable!(),
            }
        }

        if !self.bps_limiter.try_consume(1, now) {
            return Some(BlockResult::Waiting);
        }

        let next = self.next_block.take().unwrap();
        self.delayed.insert_fork(
            next.block.clone(),
            next.fork.as_ref().map(|fork| fork.hash()),
        );

        if self.bps_start.unwrap().elapsed(now) >= self.spec.rate.interval {
            self.current_bps += self.spec.rate.increment;
            self.bps_limiter.set_limit(self.current_bps);
            self.bps_start = Some(now);
        }

        Some(BlockResult::Block(next))
    }

    pub(crate) fn next_delayed(&mut self, now: Timestamp) -> Option<Block> {
        self.delayed.next(now)
    }

    pub(crate) fn published(&mut self, hash: &BlockHash, now: Timestamp) -> bool {
        if self.published.insert(*hash) {
            self.first_publish_at.get_or_insert(now);
            self.last_publish_at = Some(now);
        }
        let confirmed_before_publish = self.delayed.published(hash, now);

        if self.spec.track_confirmations
            && let Some(conf_time) = confirmed_before_publish
        {
            if self.cps_measure_start.is_none() {
                self.cps_measure_start = Some(now);
            }
            self.confirmed_recent += 1;
            self.confirmed_total += 1;
            self.sum_conf_time_recent += conf_time;
            self.sum_conf_time_total += conf_time;
            #[cfg(not(feature = "rai_protocol"))]
            self.block_factory.confirm(hash);
            #[cfg(feature = "rai_protocol")]
            self.block_factory.terminate(hash);
        }

        if !self.spec.track_confirmations {
            self.delayed.confirmed(hash, now);
            #[cfg(not(feature = "rai_protocol"))]
            self.block_factory.confirm(hash);
            #[cfg(feature = "rai_protocol")]
            self.block_factory.terminate(hash);
            self.confirmed_total += 1;
        }
        self.high_prio_tracker.published(hash, now)
    }

    pub(crate) fn publication_stats(&self) -> (usize, Duration) {
        let duration = self
            .first_publish_at
            .zip(self.last_publish_at)
            .map_or(Duration::ZERO, |(first, last)| first.elapsed(last));
        (self.published.len(), duration)
    }

    pub(crate) fn confirmed(
        &mut self,
        block_hash: &BlockHash,
        timestamp: Timestamp,
    ) -> Option<Duration> {
        if self.spec.track_confirmations {
            #[cfg(not(feature = "rai_protocol"))]
            {
                let conf_time = self.delayed.confirmed(block_hash, timestamp);
                if let Some(conf_time) = conf_time {
                    if self.cps_measure_start.is_none() {
                        self.cps_measure_start = Some(timestamp);
                    }
                    self.confirmed_recent += 1;
                    self.confirmed_total += 1;
                    self.terminated_total += 1;
                    self.sum_conf_time_recent += conf_time;
                    self.sum_conf_time_total += conf_time;
                    self.sum_termination_time_total += conf_time;
                }
                self.block_factory.confirm(block_hash);
            }
            #[cfg(feature = "rai_protocol")]
            {
                let finalized = self.block_factory.finalize(block_hash);
                for finalized_hash in finalized {
                    if let Some(primary) = self.delayed.primary_hash(&finalized_hash) {
                        // A finalization certificate also proves notarization. Record the implied
                        // termination if its websocket notification has not arrived yet.
                        self.record_termination(primary, timestamp);
                    }
                    let Some(conf_time) = self.delayed.confirmed(&finalized_hash, timestamp) else {
                        continue;
                    };
                    if self.cps_measure_start.is_none() {
                        self.cps_measure_start = Some(timestamp);
                    }
                    self.confirmed_recent += 1;
                    self.confirmed_total += 1;
                    self.sum_conf_time_recent += conf_time;
                    self.sum_conf_time_total += conf_time;
                    #[cfg(feature = "rai_protocol")]
                    {
                        self.finalization_times.insert(finalized_hash, conf_time);
                        self.finalized_at.insert(finalized_hash, timestamp);
                    }
                }
            }
        }

        self.high_prio_tracker.confirmed(block_hash, timestamp)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn record_finalization_type(&mut self, final_tally: Amount) {
        let faulty_weight = Amount::raw((Amount::MAX.number() - 1) / 5);
        let final_certificate_threshold = Amount::MAX - faulty_weight * 2;
        if final_tally >= final_certificate_threshold {
            self.final_finalized_total += 1;
        } else {
            self.fast_finalized_total += 1;
        }
    }

    pub(crate) fn reset_cps_counter(&mut self, now: Timestamp) {
        self.confirmed_recent = 0;
        self.sum_conf_time_recent = Duration::ZERO;
        self.cps_measure_start = Some(now);
    }

    pub(crate) fn cps(&self, now: Timestamp) -> i32 {
        match self.cps_measure_start {
            Some(start) => (self.confirmed_recent as f64 / start.elapsed(now).as_secs_f64()) as i32,
            None => 0,
        }
    }

    pub(crate) fn average_conf_time(&self) -> Duration {
        if self.confirmed_recent == 0 {
            Duration::ZERO
        } else {
            self.sum_conf_time_recent / self.confirmed_recent as u32
        }
    }

    pub(crate) fn unconfirmed_hashes(&self) -> Vec<BlockHash> {
        self.delayed.hashes()
    }

    pub(crate) fn stats(&self, now: Timestamp) -> SpamStats {
        SpamStats {
            total_confirmed: self.confirmed_total,
            target_bps: self.current_bps,
            current_cps: self.cps(now),
            average_conf_time: self.average_conf_time(),
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn cut_reported(
        &mut self,
        node_index: usize,
        epoch: u64,
        cut_hash: Blake2Hash,
        reclassified_elections: usize,
        cut: HashSet<BlockHash>,
        non_cut: HashSet<BlockHash>,
        timestamp: Timestamp,
    ) {
        self.cut_reports
            .entry(epoch)
            .or_default()
            .insert(node_index, (cut_hash, timestamp));
        if node_index == 0 {
            self.cuts_by_epoch.insert(
                epoch,
                EpochCutState {
                    cut,
                    non_cut,
                    installed_at: timestamp,
                    reclassified_elections,
                },
            );
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn epoch_reported(
        &mut self,
        node_index: usize,
        epoch: u64,
        round: u32,
        non_cut_count: u64,
        finalized_hash: String,
        timestamp: Timestamp,
        prs: usize,
    ) {
        if self.completed_epoch_ids.contains(&epoch) || self.epoch_error.is_some() {
            return;
        }
        let reports = self.epoch_reports.entry(epoch).or_default();
        let value = (non_cut_count, finalized_hash, round, timestamp);
        if let Some(previous) = reports.insert(node_index, value.clone())
            && (previous.0 != value.0 || previous.1 != value.1 || previous.2 != value.2)
        {
            self.epoch_error = Some(format!(
                "PR{node_index} emitted conflicting completion reports for RAI epoch {epoch}"
            ));
            return;
        }
        if reports.len() != prs {
            return;
        }
        let first = reports.values().next().unwrap();
        if reports
            .values()
            .any(|report| report.0 != first.0 || report.1 != first.1)
        {
            self.epoch_error = Some(format!(
                "RAI epoch {epoch} closed with different finalized blocks across PRs"
            ));
            return;
        }
        self.completed_epoch_ids.insert(epoch);
        self.epochs_completed += 1;
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn epochs_completed(&self) -> usize {
        self.epochs_completed
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn epoch_error(&self) -> Option<&str> {
        self.epoch_error.as_deref()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn epoch_stats(&self, prs: usize) -> Vec<EpochStats> {
        let mut epochs: Vec<_> = self.cuts_by_epoch.keys().copied().collect();
        epochs.sort_unstable();
        epochs
            .into_iter()
            .map(|epoch| {
                let cut = &self.cuts_by_epoch[&epoch];
                let reports = self.cut_reports.get(&epoch);
                let completions = self.epoch_reports.get(&epoch);
                let convergence = |timestamps: Vec<Timestamp>| {
                    let first = timestamps.iter().min().copied();
                    let last = timestamps.iter().max().copied();
                    first
                        .zip(last)
                        .map_or(Duration::ZERO, |(a, b)| a.elapsed(b))
                };
                let cut_hashes: HashSet<_> = reports
                    .into_iter()
                    .flat_map(|values| values.values().map(|(hash, _)| *hash))
                    .collect();
                let cut_hash = cut_hashes.iter().next().copied().unwrap_or_default();
                let epoch_hash = completions
                    .and_then(|values| values.values().next())
                    .map(|(_, hash, _, _)| hash.clone())
                    .unwrap_or_default();
                EpochStats {
                    epoch,
                    cut: self.group_stats(&cut.cut, cut.installed_at),
                    non_cut: self.group_stats(&cut.non_cut, cut.installed_at),
                    cut_hash,
                    reclassified_elections: cut.reclassified_elections,
                    cut_hash_convergence: convergence(
                        reports
                            .into_iter()
                            .flat_map(|values| values.values().map(|(_, at)| *at))
                            .collect(),
                    ),
                    cut_reports: reports.map_or(0, HashMap::len),
                    cut_hashes_agree: cut_hashes.len() <= 1,
                    epoch_hash,
                    epoch_hash_convergence: convergence(
                        completions
                            .into_iter()
                            .flat_map(|values| values.values().map(|(_, _, _, at)| *at))
                            .collect(),
                    ),
                    epoch_completion_time: completions
                        .into_iter()
                        .flat_map(|values| values.values().map(|(_, _, _, at)| *at))
                        .max()
                        .map_or(Duration::ZERO, |at| cut.installed_at.elapsed(at)),
                    epoch_reports: completions.map_or(0, HashMap::len),
                    convergence_rounds: completions
                        .into_iter()
                        .flat_map(|values| values.values().map(|(_, _, round, _)| *round + 1))
                        .max()
                        .unwrap_or_default(),
                    expected_reports: prs,
                }
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    fn group_stats(&self, group: &HashSet<BlockHash>, cut_at: Timestamp) -> GroupStats {
        let mut times: Vec<_> = group
            .iter()
            .filter_map(|hash| self.finalization_times.get(hash))
            .copied()
            .collect();
        times.sort_unstable();
        let finalized = times.len();
        let total = group.len();
        let average_latency = if finalized == 0 {
            Duration::ZERO
        } else {
            times.iter().sum::<Duration>() / finalized as u32
        };
        GroupStats {
            total,
            finalized,
            confirmation_percent: if total == 0 {
                0.0
            } else {
                finalized as f64 * 100.0 / total as f64
            },
            completion_time: group
                .iter()
                .filter_map(|hash| self.finalized_at.get(hash))
                .filter(|at| **at > cut_at)
                .max()
                .map_or(Duration::ZERO, |at| cut_at.elapsed(*at)),
            average_latency,
            p50_latency: percentile(&times, 50),
            p95_latency: percentile(&times, 95),
            p99_latency: percentile(&times, 99),
            max_latency: times.last().copied().unwrap_or_default(),
        }
    }
}

#[cfg(feature = "rai_protocol")]
pub(crate) struct GroupStats {
    pub total: usize,
    pub finalized: usize,
    pub confirmation_percent: f64,
    pub completion_time: Duration,
    pub average_latency: Duration,
    pub p50_latency: Duration,
    pub p95_latency: Duration,
    pub p99_latency: Duration,
    pub max_latency: Duration,
}

#[cfg(feature = "rai_protocol")]
struct EpochCutState {
    cut: HashSet<BlockHash>,
    non_cut: HashSet<BlockHash>,
    installed_at: Timestamp,
    reclassified_elections: usize,
}

#[cfg(feature = "rai_protocol")]
pub(crate) struct EpochStats {
    pub epoch: u64,
    pub cut: GroupStats,
    pub non_cut: GroupStats,
    pub cut_hash: Blake2Hash,
    pub reclassified_elections: usize,
    pub cut_hash_convergence: Duration,
    pub cut_reports: usize,
    pub cut_hashes_agree: bool,
    pub epoch_hash: String,
    pub epoch_hash_convergence: Duration,
    pub epoch_completion_time: Duration,
    pub epoch_reports: usize,
    pub convergence_rounds: u32,
    pub expected_reports: usize,
}

pub(crate) struct SpamStats {
    pub(crate) total_confirmed: usize,
    pub(crate) target_bps: usize,
    pub(crate) current_cps: i32,
    pub(crate) average_conf_time: Duration,
}
