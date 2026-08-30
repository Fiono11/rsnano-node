use crate::domain::{
    AccountMap, BlockFactory, BlockResult, DelayedBlocks, Forks, RateSpec, SpamStrategy,
    high_prio_tracker::HighPrioTracker,
};
use rsnano_network::token_bucket::TokenBucketLogic;
use rsnano_nullable_clock::Timestamp;
#[cfg(feature = "rai_protocol")]
use rsnano_types::Amount;
use rsnano_types::{Block, BlockHash};
#[cfg(feature = "rai_protocol")]
use std::collections::{BTreeMap, HashMap};
use std::{collections::HashSet, time::Duration};

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Default)]
pub(crate) struct EpochStats {
    pub(crate) published: usize,
    pub(crate) terminated: usize,
    pub(crate) finalized: usize,
    pub(crate) fast_finalized: usize,
    pub(crate) final_vote_finalized: usize,
    pub(crate) sum_confirmation_time: Duration,
    pub(crate) sum_termination_time: Duration,
}

#[cfg(feature = "rai_protocol")]
#[derive(Clone, Default)]
pub(crate) struct PerPrEpochStats {
    pub(crate) finalized_hashes: HashSet<BlockHash>,
    pub(crate) fast_finalized: usize,
    pub(crate) final_vote_finalized: usize,
    pub(crate) sum_confirmation_time: Duration,
}

pub(crate) struct SpamSpec {
    pub(crate) spam_strategy: SpamStrategy,
    pub(crate) max_blocks: usize,
    pub(crate) rate: RateSpec,
    pub(crate) fork_probability: f64,
    pub(crate) track_confirmations: bool,
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
    #[cfg(feature = "rai_protocol")]
    pub(crate) epoch_stats: BTreeMap<u64, EpochStats>,
    #[cfg(feature = "rai_protocol")]
    pub(crate) unclassified_finalizations: usize,
    #[cfg(feature = "rai_protocol")]
    pub(crate) per_pr_epoch_stats: BTreeMap<(usize, u64), PerPrEpochStats>,
    #[cfg(feature = "rai_protocol")]
    pub(crate) per_pr_unclassified: BTreeMap<usize, HashSet<BlockHash>>,
    #[cfg(feature = "rai_protocol")]
    per_pr_finalized: BTreeMap<usize, HashSet<BlockHash>>,
    #[cfg(feature = "rai_protocol")]
    per_pr_canonical_epochs: BTreeMap<(usize, BlockHash), u64>,
    #[cfg(feature = "rai_protocol")]
    pub(crate) expected_hashes: HashSet<BlockHash>,
    #[cfg(feature = "rai_protocol")]
    published_at: HashMap<BlockHash, Timestamp>,
    pub(crate) sum_termination_time_total: Duration,
    terminated: HashSet<BlockHash>,
    pub(crate) cps_measure_start: Option<Timestamp>,
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
            #[cfg(feature = "rai_protocol")]
            epoch_stats: Default::default(),
            #[cfg(feature = "rai_protocol")]
            unclassified_finalizations: 0,
            #[cfg(feature = "rai_protocol")]
            per_pr_epoch_stats: Default::default(),
            #[cfg(feature = "rai_protocol")]
            per_pr_unclassified: Default::default(),
            #[cfg(feature = "rai_protocol")]
            per_pr_finalized: Default::default(),
            #[cfg(feature = "rai_protocol")]
            per_pr_canonical_epochs: Default::default(),
            #[cfg(feature = "rai_protocol")]
            expected_hashes: Default::default(),
            #[cfg(feature = "rai_protocol")]
            published_at: Default::default(),
            sum_termination_time_total: Duration::ZERO,
            terminated: HashSet::new(),
            cps_measure_start: None,
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        let max_blocks = self.block_factory.max_blocks();
        #[cfg(feature = "rai_protocol")]
        {
            // Termination/notarization releases dependent block generation, but it is not
            // finality. Keep the run alive until every requested block has actually been
            // finalized (or the outer timeout cancels it).
            max_blocks > 0 && self.confirmed_total >= max_blocks
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            max_blocks > 0
                && (self.confirmed_total >= max_blocks
                    || (self.block_factory.created() >= max_blocks && self.delayed.len() == 0))
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn all_blocks_published(&self) -> bool {
        let max_blocks = self.block_factory.max_blocks();
        max_blocks > 0 && self.expected_hashes.len() >= max_blocks
    }

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
        #[cfg(feature = "rai_protocol")]
        {
            self.expected_hashes.insert(*hash);
            self.published_at.entry(*hash).or_insert(now);
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
            self.block_factory.terminate(hash);
        }

        if !self.spec.track_confirmations {
            self.delayed.confirmed(hash, now);
            self.block_factory.terminate(hash);
            self.confirmed_total += 1;
        }
        self.high_prio_tracker.published(hash, now)
    }

    pub(crate) fn confirmed(
        &mut self,
        block_hash: &BlockHash,
        timestamp: Timestamp,
        #[cfg(feature = "rai_protocol")] finalization_epoch: Option<u64>,
    ) -> Option<Duration> {
        if self.spec.track_confirmations {
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
                if let Some(epoch) = finalization_epoch {
                    let stats = self.epoch_stats.entry(epoch).or_default();
                    stats.finalized += 1;
                    stats.sum_confirmation_time += conf_time;
                } else {
                    self.unclassified_finalizations += 1;
                }
            }
        }

        self.high_prio_tracker.confirmed(block_hash, timestamp)
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn record_finalization_type(&mut self, epoch: u64, final_tally: Amount) {
        let faulty_weight = Amount::raw((Amount::MAX.number() - 1) / 5);
        let final_certificate_threshold = Amount::MAX - faulty_weight * 2;
        if final_tally >= final_certificate_threshold {
            self.final_finalized_total += 1;
            self.epoch_stats
                .entry(epoch)
                .or_default()
                .final_vote_finalized += 1;
        } else {
            self.fast_finalized_total += 1;
            self.epoch_stats.entry(epoch).or_default().fast_finalized += 1;
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn record_pr_finalization(
        &mut self,
        pr_index: usize,
        block_hash: BlockHash,
        epoch: Option<u64>,
        final_tally: Amount,
        timestamp: Timestamp,
    ) {
        if self.expected_hashes.contains(&block_hash) {
            self.per_pr_finalized
                .entry(pr_index)
                .or_default()
                .insert(block_hash);
        }
        let Some(epoch) = epoch else {
            self.per_pr_unclassified
                .entry(pr_index)
                .or_default()
                .insert(block_hash);
            return;
        };
        let stats = self
            .per_pr_epoch_stats
            .entry((pr_index, epoch))
            .or_default();
        if !stats.finalized_hashes.insert(block_hash) {
            return;
        }
        if let Some(published) = self.published_at.get(&block_hash) {
            stats.sum_confirmation_time += published.elapsed(timestamp);
        }
        let faulty_weight = Amount::raw((Amount::MAX.number() - 1) / 5);
        let final_certificate_threshold = Amount::MAX - faulty_weight * 2;
        if final_tally >= final_certificate_threshold {
            stats.final_vote_finalized += 1;
        } else {
            stats.fast_finalized += 1;
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn record_node_finalization(
        &mut self,
        pr_index: usize,
        block_hash: BlockHash,
        epoch: u64,
        final_tally: Amount,
        duration: Duration,
    ) {
        if !self.expected_hashes.contains(&block_hash) {
            return;
        }
        let newly_finalized_hash = self
            .per_pr_finalized
            .entry(pr_index)
            .or_default()
            .insert(block_hash);
        self.per_pr_canonical_epochs
            .entry((pr_index, block_hash))
            .and_modify(|canonical| *canonical = (*canonical).min(epoch))
            .or_insert(epoch);
        let stats = self
            .per_pr_epoch_stats
            .entry((pr_index, epoch))
            .or_default();
        if !stats.finalized_hashes.insert(block_hash) {
            return;
        }
        stats.sum_confirmation_time += duration;
        let faulty_weight = Amount::raw((Amount::MAX.number() - 1) / 5);
        let final_certificate_threshold = Amount::MAX - faulty_weight * 2;
        if final_tally >= final_certificate_threshold {
            stats.final_vote_finalized += 1;
        } else {
            stats.fast_finalized += 1;
        }

        if pr_index == 0 {
            if newly_finalized_hash {
                self.confirmed_total += 1;
                self.sum_conf_time_total += duration;
            }
            let epoch_stats = self.epoch_stats.entry(epoch).or_default();
            epoch_stats.finalized += 1;
            epoch_stats.sum_confirmation_time += duration;
            if final_tally >= final_certificate_threshold {
                self.final_finalized_total += 1;
                epoch_stats.final_vote_finalized += 1;
            } else {
                self.fast_finalized_total += 1;
                epoch_stats.fast_finalized += 1;
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn clear_pr_finalizations(&mut self) {
        self.per_pr_epoch_stats.clear();
        self.per_pr_unclassified.clear();
        self.per_pr_finalized.clear();
        self.per_pr_canonical_epochs.clear();
        self.confirmed_total = 0;
        self.sum_conf_time_total = Duration::ZERO;
        self.fast_finalized_total = 0;
        self.final_finalized_total = 0;
        self.unclassified_finalizations = 0;
        for stats in self.epoch_stats.values_mut() {
            stats.finalized = 0;
            stats.fast_finalized = 0;
            stats.final_vote_finalized = 0;
            stats.sum_confirmation_time = Duration::ZERO;
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn pr_finalized_hashes(&self, pr_index: usize) -> HashSet<BlockHash> {
        self.per_pr_finalized
            .get(&pr_index)
            .cloned()
            .unwrap_or_default()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn all_prs_finalized(&self, prs: usize) -> bool {
        !self.expected_hashes.is_empty()
            && self.block_factory.max_blocks() > 0
            && self.block_factory.created() >= self.block_factory.max_blocks()
            && (0..prs).all(|pr| {
                self.per_pr_finalized
                    .get(&pr)
                    .is_some_and(|hashes| hashes.len() == self.expected_hashes.len())
            })
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn canonical_epoch_hashes(&self, pr_index: usize, epoch: u64) -> HashSet<BlockHash> {
        self.per_pr_canonical_epochs
            .iter()
            .filter_map(|((pr, hash), canonical)| {
                (*pr == pr_index && *canonical == epoch).then_some(*hash)
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn canonical_epoch(&self, pr_index: usize, hash: &BlockHash) -> Option<u64> {
        self.per_pr_canonical_epochs
            .get(&(pr_index, *hash))
            .copied()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn finalized_epochs(&self, pr_index: usize, hash: &BlockHash) -> Vec<u64> {
        self.per_pr_epoch_stats
            .iter()
            .filter_map(|((pr, epoch), stats)| {
                (*pr == pr_index && stats.finalized_hashes.contains(hash)).then_some(*epoch)
            })
            .collect()
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn all_prs_same_canonical_epochs(&self, prs: usize) -> bool {
        self.all_prs_finalized(prs)
            && self.expected_hashes.iter().all(|hash| {
                let expected = self.per_pr_canonical_epochs.get(&(0, *hash));
                expected.is_some()
                    && (1..prs).all(|pr| self.per_pr_canonical_epochs.get(&(pr, *hash)) == expected)
            })
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
}

pub(crate) struct SpamStats {
    pub(crate) total_confirmed: usize,
    pub(crate) target_bps: usize,
    pub(crate) current_cps: i32,
    pub(crate) average_conf_time: Duration,
}
