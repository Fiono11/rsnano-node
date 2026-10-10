use crate::domain::{
    AccountMap, BlockFactory, BlockResult, DelayedBlocks, Forks, RateSpec, Representatives,
    SpamStrategy, high_prio_tracker::HighPrioTracker,
};
use rsnano_network::token_bucket::TokenBucketLogic;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Block, BlockHash};
use rustc_hash::{FxHashMap, FxHashSet};
use std::{collections::BTreeMap, time::Duration};

/// RAI: how long an owner waits for an unconfirmed non-fork block before
/// extending it with a fresh child (the paper's retry-or-fresh-child policy)
const CONTINUE_AFTER: Duration = Duration::from_secs(5);

pub(crate) struct SpamSpec {
    pub(crate) spam_strategy: SpamStrategy,
    pub(crate) max_blocks: usize,
    pub(crate) rate: RateSpec,
    pub(crate) fork_probability: f64,
    pub(crate) track_confirmations: bool,
    /// RAI: the representatives the spam accounts delegate to
    pub(crate) representatives: Representatives,
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
    pub(crate) cps_measure_start: Option<Timestamp>,
    /// A fork alternative's hash -> the primary it stands in for: either
    /// block of a fork position confirms the position once
    confirmation_aliases: FxHashMap<BlockHash, BlockHash>,
    /// The primaries published with a fork alternative, until one of the
    /// two is confirmed: they are measured apart from the non-fork blocks
    fork_primaries: FxHashSet<BlockHash>,
    pub(crate) fork_created: usize,
    pub(crate) fork_confirmed: usize,
    /// Fork positions confirmed through the alternative, not the primary
    pub(crate) alternative_confirmed: usize,
    pub(crate) nonfork_created: usize,
    pub(crate) nonfork_confirmed: usize,
    /// Confirmation latency of every confirmed position, in ms
    pub(crate) confirmation_histogram_ms: BTreeMap<u64, usize>,
    /// Confirmation latency of the blocks published without a fork, in ms
    pub(crate) nonfork_histogram_ms: BTreeMap<u64, usize>,
    /// When the last non-fork block was confirmed
    pub(crate) nonfork_done_at: Option<Timestamp>,
    /// When the unconfirmed blocks were last scanned for owner continuation
    continuation_scanned: Option<Timestamp>,
    /// Evaluation time series, per second since the first publication:
    /// the non-fork blocks published, and the non-fork blocks confirmed
    /// in that second with their confirmation latencies in ms
    series_start: Option<Timestamp>,
    pub(crate) series_start_unix_ms: Option<u64>,
    published_series: BTreeMap<u64, usize>,
    nonfork_series: BTreeMap<u64, BTreeMap<u64, usize>>,
}

impl SpamLogic {
    pub(crate) fn new(account_map: AccountMap, spec: SpamSpec) -> Self {
        Self {
            delayed: Default::default(),
            continuation_scanned: None,
            high_prio_tracker: Default::default(),
            block_factory: BlockFactory::new(
                account_map,
                spec.max_blocks,
                spec.spam_strategy,
                spec.representatives.clone(),
            ),
            current_bps: spec.rate.initial_bps,
            bps_limiter: TokenBucketLogic::new(spec.rate.initial_bps),
            next_block: None,
            bps_start: None,
            spec,
            confirmed_total: 0,
            confirmed_recent: 0,
            sum_conf_time_recent: Duration::ZERO,
            sum_conf_time_total: Duration::ZERO,
            cps_measure_start: None,
            confirmation_aliases: Default::default(),
            fork_primaries: Default::default(),
            fork_created: 0,
            fork_confirmed: 0,
            alternative_confirmed: 0,
            nonfork_created: 0,
            nonfork_confirmed: 0,
            confirmation_histogram_ms: Default::default(),
            nonfork_histogram_ms: Default::default(),
            nonfork_done_at: None,
            series_start: None,
            series_start_unix_ms: None,
            published_series: Default::default(),
            nonfork_series: Default::default(),
        }
    }

    /// The second of the evaluation time series a moment falls in, starting
    /// the series at the first publication
    fn series_second(&mut self, now: Timestamp) -> u64 {
        let start = *self.series_start.get_or_insert_with(|| {
            self.series_start_unix_ms = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            );
            now
        });
        start.elapsed(now).as_secs()
    }

    /// The evaluation time series as JSON rows `[second, published,
    /// confirmed, p50_ms, p95_ms]`, one per second from the first
    /// publication to the last non-fork confirmation
    pub(crate) fn series_json(&self) -> serde_json::Value {
        let last = self
            .published_series
            .keys()
            .chain(self.nonfork_series.keys())
            .max()
            .copied();
        let rows: Vec<serde_json::Value> = (0..=last.unwrap_or(0))
            .map(|second| {
                let published = self.published_series.get(&second).copied().unwrap_or(0);
                let (confirmed, p50, p95) = match self.nonfork_series.get(&second) {
                    Some(histogram) => {
                        let total: usize = histogram.values().sum();
                        (
                            total,
                            quantile(histogram, total, 0.5),
                            quantile(histogram, total, 0.95),
                        )
                    }
                    None => (0, None, None),
                };
                serde_json::json!([second, published, confirmed, p50, p95])
            })
            .collect();
        serde_json::Value::Array(rows)
    }

    /// Every block is created and every non-fork block is confirmed. A fork
    /// position may never resolve (an exact split of first votes leaves it
    /// retained), so forks do not hold the run open; they are counted apart.
    pub(crate) fn is_finished(&self) -> bool {
        if self.block_factory.max_blocks() == 0 {
            return false;
        }
        if !self.spec.track_confirmations {
            return self.confirmed_total >= self.block_factory.max_blocks();
        }
        self.block_factory.created() >= self.block_factory.max_blocks()
            && self.next_block.is_none()
            && self.nonfork_confirmed >= self.nonfork_created
    }

    pub(crate) fn fork_propability(&self) -> f64 {
        self.spec.fork_probability
    }

    pub(crate) fn next_block(&mut self, is_fork: bool, now: Timestamp) -> Option<BlockResult> {
        if self.bps_start.is_none() {
            self.bps_start = Some(now);
        }

        if self.next_block.is_none() {
            if self.block_factory.max_blocks_reached() {
                return None;
            }
            // RAI, "Fresh-child recovery": the owner's persistent submission
            // extends an unconfirmed non-fork frontier that waited this long;
            // the scan over the unconfirmed blocks runs twice a second
            if self
                .continuation_scanned
                .is_none_or(|last| last.elapsed(now) >= Duration::from_millis(500))
            {
                self.continuation_scanned = Some(now);
                for hash in self.delayed.stale(now, CONTINUE_AFTER) {
                    self.block_factory.allow_continuation(&hash);
                }
            }

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
        if let Some(fork) = &next.fork {
            self.confirmation_aliases
                .insert(fork.hash(), next.block.hash());
            self.fork_primaries.insert(next.block.hash());
            self.fork_created += 1;
        } else {
            self.nonfork_created += 1;
        }
        self.delayed.insert(next.block.clone());

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
        if self.delayed.first_publish(hash).is_none() {
            let second = self.series_second(now);
            *self.published_series.entry(second).or_default() += 1;
        }
        self.delayed.published(hash, now);

        if !self.spec.track_confirmations {
            self.delayed.confirmed(hash, now);
            self.block_factory.confirm(hash);
            self.confirmed_total += 1;
        }
        self.high_prio_tracker.published(hash, now)
    }

    pub(crate) fn confirmed(
        &mut self,
        block_hash: &BlockHash,
        timestamp: Timestamp,
    ) -> Option<Duration> {
        if self.spec.track_confirmations {
            let tracked = self
                .confirmation_aliases
                .remove(block_hash)
                .unwrap_or(*block_hash);
            let conf_time = self.delayed.confirmed(&tracked, timestamp);

            if let Some(conf_time) = conf_time {
                if tracked != *block_hash {
                    self.alternative_confirmed += 1;
                }
                if self.cps_measure_start.is_none() {
                    self.cps_measure_start = Some(timestamp);
                }
                self.confirmed_recent += 1;
                self.confirmed_total += 1;
                self.sum_conf_time_recent += conf_time;
                self.sum_conf_time_total += conf_time;
                let ms = conf_time.as_millis() as u64;
                *self.confirmation_histogram_ms.entry(ms).or_default() += 1;
                if self.fork_primaries.remove(&tracked) {
                    self.fork_confirmed += 1;
                } else {
                    self.nonfork_confirmed += 1;
                    *self.nonfork_histogram_ms.entry(ms).or_default() += 1;
                    self.nonfork_done_at = Some(timestamp);
                    let second = self.series_second(timestamp);
                    *self
                        .nonfork_series
                        .entry(second)
                        .or_default()
                        .entry(ms)
                        .or_default() += 1;
                }
            }
            self.block_factory.confirm(block_hash);
        }

        self.high_prio_tracker.confirmed(block_hash, timestamp)
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

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{Amount, BlockHash, PrivateKey};

    /// Either block of a fork position confirms it once, and a fork is
    /// counted apart from the non-fork blocks
    #[test]
    fn a_fork_alternative_confirms_its_position_once() {
        let primary = test_block(1, 2);
        let alternative = test_block(1, 3);
        let mut logic = test_logic(1, 1.0);
        logic.next_block = Some(Forks::new_fork(primary.clone(), alternative.clone()));
        let now = Timestamp::new_test_instance();
        assert!(matches!(
            logic.next_block(true, now),
            Some(BlockResult::Block(_))
        ));
        logic.published(&primary.hash(), now);
        logic.confirmed(&alternative.hash(), now + Duration::from_millis(7));
        logic.confirmed(&alternative.hash(), now + Duration::from_secs(12));
        logic.confirmed(&primary.hash(), now + Duration::from_secs(13));
        assert_eq!(logic.confirmed_total, 1);
        assert_eq!(logic.alternative_confirmed, 1);
        assert_eq!(logic.confirmation_histogram_ms.get(&7), Some(&1));
        assert_eq!((logic.fork_created, logic.fork_confirmed), (1, 1));
        assert_eq!((logic.nonfork_created, logic.nonfork_confirmed), (0, 0));
        assert!(logic.nonfork_histogram_ms.is_empty());
    }

    /// The run ends once every non-fork block is confirmed: an unresolved
    /// fork does not hold it open, and a fork's confirmation does not enter
    /// the non-fork latency
    #[test]
    fn an_unresolved_fork_does_not_hold_the_run_open() {
        let mut logic = test_logic(0, 0.5);
        let now = Timestamp::new_test_instance();
        let plain = test_block(1, 2);
        let primary = test_block(2, 2);
        let alternative = test_block(2, 3);
        logic.next_block = Some(Forks::new(plain.clone()));
        assert!(matches!(
            logic.next_block(false, now),
            Some(BlockResult::Block(_))
        ));
        logic.next_block = Some(Forks::new_fork(primary.clone(), alternative));
        assert!(matches!(
            logic.next_block(true, now),
            Some(BlockResult::Block(_))
        ));
        logic.published(&plain.hash(), now);
        logic.published(&primary.hash(), now);
        logic.confirmed(&plain.hash(), now + Duration::from_millis(5));
        assert_eq!((logic.nonfork_created, logic.nonfork_confirmed), (1, 1));
        assert_eq!((logic.fork_created, logic.fork_confirmed), (1, 0));
        assert_eq!(logic.nonfork_histogram_ms.get(&5), Some(&1));
        assert_eq!(logic.nonfork_done_at, Some(now + Duration::from_millis(5)));
    }

    #[test]
    fn rate_limited_last_block_is_not_dropped() {
        let mut accounts = AccountMap::default();
        let initial_key = PrivateKey::from(1);
        accounts.add_unopened(initial_key.clone());
        accounts.add_unopened(PrivateKey::from(2));
        accounts.set_account_state(initial_key.account(), Amount::nano(1), BlockHash::from(1));
        let mut logic = SpamLogic::new(
            accounts,
            SpamSpec {
                spam_strategy: SpamStrategy::SendReceive,
                max_blocks: 2,
                rate: RateSpec::new(1),
                fork_probability: 0.0,
                track_confirmations: true,
                representatives: Representatives::default(),
            },
        );
        let now = Timestamp::new_test_instance();

        let first = logic.next_block(false, now).unwrap();
        let BlockResult::Block(first) = first else {
            panic!("first block should not be rate limited");
        };
        let first_hash = first.block.hash();
        logic.published(&first_hash, now);
        logic.confirmed(&first_hash, now);

        assert!(matches!(
            logic.next_block(false, now),
            Some(BlockResult::Waiting)
        ));
        assert!(matches!(
            logic.next_block(false, now + Duration::from_secs(1)),
            Some(BlockResult::Block(_))
        ));
    }

    /*
     * Test helpers
     */

    fn test_block(previous: u64, representative: u64) -> Block {
        use rsnano_types::{Link, PublicKey, StateBlockArgs, WorkNonce};
        Block::from(StateBlockArgs {
            key: &PrivateKey::from(1),
            previous: BlockHash::from(previous),
            representative: PublicKey::from(representative),
            balance: Amount::nano(5),
            link: Link::ZERO,
            work: WorkNonce::new(0),
        })
    }

    fn test_logic(max_blocks: usize, fork_probability: f64) -> SpamLogic {
        SpamLogic::new(
            AccountMap::default(),
            SpamSpec {
                spam_strategy: SpamStrategy::SendReceive,
                max_blocks,
                rate: RateSpec::new(100),
                fork_probability,
                track_confirmations: true,
                representatives: Representatives::default(),
            },
        )
    }
}

/// The q-quantile of a {ms: count} histogram holding `total` samples
fn quantile(histogram: &BTreeMap<u64, usize>, total: usize, q: f64) -> Option<u64> {
    if total == 0 {
        return None;
    }
    let rank = (q * total as f64).ceil() as usize;
    let mut seen = 0;
    for (ms, count) in histogram {
        seen += count;
        if seen >= rank {
            return Some(*ms);
        }
    }
    histogram.keys().next_back().copied()
}
