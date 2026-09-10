use crate::domain::{
    AccountMap, BlockFactory, BlockResult, DelayedBlocks, Forks, RateSpec, SpamStrategy,
    high_prio_tracker::HighPrioTracker,
};
use rsnano_network::token_bucket::TokenBucketLogic;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Block, BlockHash};
use std::time::Duration;

pub(crate) struct SpamSpec {
    pub(crate) spam_strategy: SpamStrategy,
    pub(crate) max_blocks: usize,
    pub(crate) rate: RateSpec,
    pub(crate) fork_probability: f64,
    pub(crate) track_confirmations: bool,
}

pub(crate) struct SpamLogic {
    pub confirmed_forks: usize,
    pub sum_fork_time: Duration,
    pub sum_nonfork_time: Duration,
    fork_originals: std::collections::HashMap<BlockHash, BlockHash>,
    fork_hashes: std::collections::HashSet<BlockHash>,
    pub workload_records: Vec<(rsnano_types::QualifiedRoot, BlockHash, Option<BlockHash>)>,
    pub published_hashes: std::collections::HashSet<BlockHash>,
    unpublished_hashes: std::collections::HashSet<BlockHash>,
    pub published_blocks: usize,

    pub deadline: Option<std::time::Instant>,
    pub workload_roots: std::collections::HashSet<rsnano_types::QualifiedRoot>,
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
}

impl SpamLogic {
    pub(crate) fn new(account_map: AccountMap, spec: SpamSpec) -> Self {
        Self {
            confirmed_forks: 0,
            sum_fork_time: Duration::ZERO,
            sum_nonfork_time: Duration::ZERO,
            fork_originals: Default::default(),
            fork_hashes: Default::default(),
            workload_records: Vec::new(),
            published_hashes: Default::default(),
            unpublished_hashes: Default::default(),
            published_blocks: 0,
            deadline: None,
            workload_roots: Default::default(),
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
            cps_measure_start: None,
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        if let Some(deadline) = self.deadline {
            let now = std::time::Instant::now();
            return now >= deadline + Duration::from_secs(240)
                || (now >= deadline
                    && (self.spec.max_blocks == 0
                        || self.published_blocks >= self.spec.max_blocks));
        }
        self.block_factory.max_blocks() > 0
            && self.confirmed_total >= self.block_factory.max_blocks()
    }

    pub(crate) fn fork_propability(&self) -> f64 {
        self.spec.fork_probability
    }

    pub(crate) fn next_block(&mut self, is_fork: bool, now: Timestamp) -> Option<BlockResult> {
        if self.deadline.is_some_and(|d| {
            std::time::Instant::now()
                >= d + if self.spec.max_blocks == 0 {
                    Duration::ZERO
                } else {
                    Duration::from_secs(240)
                }
        }) {
            return None;
        }
        if self.bps_start.is_none() {
            self.bps_start = Some(now);
        }

        if self.next_block.is_none() {
            if self.block_factory.max_blocks_reached() {
                return None;
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
        self.unpublished_hashes.insert(next.block.hash());
        self.workload_records.push((
            next.block.qualified_root(),
            next.block.hash(),
            next.fork.as_ref().map(|b| b.hash()),
        ));
        self.workload_roots.insert(next.block.qualified_root());
        if let Some(fork) = &next.fork {
            self.fork_originals.insert(fork.hash(), next.block.hash());
            self.fork_hashes.insert(next.block.hash());
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
        self.published_hashes.insert(*hash);
        if self
            .unpublished_hashes
            .remove(self.fork_originals.get(hash).unwrap_or(hash))
        {
            self.published_blocks += 1;
        }
        self.delayed
            .published(self.fork_originals.get(hash).unwrap_or(hash), now);

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
            let original = self.fork_originals.get(block_hash).unwrap_or(block_hash);
            let is_fork = self.fork_hashes.contains(original);
            let conf_time = self.delayed.confirmed(original, timestamp);

            if let Some(conf_time) = conf_time {
                if self.cps_measure_start.is_none() {
                    self.cps_measure_start = Some(timestamp);
                }
                self.confirmed_recent += 1;
                self.confirmed_total += 1;
                self.sum_conf_time_recent += conf_time;
                self.sum_conf_time_total += conf_time;
                if is_fork {
                    self.confirmed_forks += 1;
                    self.sum_fork_time += conf_time;
                } else {
                    self.sum_nonfork_time += conf_time;
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

    #[test]
    fn measurement_does_not_stop_before_all_requested_blocks_are_published() {
        let mut logic = SpamLogic::new(
            AccountMap::default(),
            SpamSpec {
                spam_strategy: SpamStrategy::SendReceive,
                max_blocks: 2,
                rate: RateSpec::new(2000),
                fork_probability: 0.0,
                track_confirmations: true,
            },
        );
        logic.deadline = Some(std::time::Instant::now() - Duration::from_secs(1));
        logic.published_blocks = 1;
        assert!(!logic.is_finished());
        logic.published_blocks = 2;
        assert!(logic.is_finished());
    }

    #[test]
    fn either_fork_winner_is_counted_once_from_first_publication() {
        for fork_wins in [false, true] {
            let mut accounts = AccountMap::default();
            let key = PrivateKey::from(1);
            accounts.add_unopened(key.clone());
            accounts.add_unopened(PrivateKey::from(2));
            accounts.set_account_state(key.account(), Amount::nano(1), BlockHash::from(1));
            let mut logic = SpamLogic::new(
                accounts,
                SpamSpec {
                    spam_strategy: SpamStrategy::SendReceive,
                    max_blocks: 3,
                    rate: RateSpec::new(1),
                    fork_probability: 1.0,
                    track_confirmations: true,
                },
            );
            // Complete fork-free funding before testing a forked workload block.
            for _ in 0..2 {
                let BlockResult::Block(block) = logic.block_factory.create_next(false).unwrap()
                else {
                    panic!("funding block missing")
                };
                logic.block_factory.confirm(&block.block.hash());
            }
            let now = Timestamp::new_test_instance();
            let BlockResult::Block(blocks) = logic.next_block(true, now).unwrap() else {
                panic!("expected block");
            };
            let original = blocks.block.hash();
            let fork = blocks.fork.unwrap().hash();
            logic.published(&original, now);
            logic.published(&fork, now + Duration::from_millis(1));
            let winner = if fork_wins { fork } else { original };
            logic.confirmed(&winner, now + Duration::from_millis(10));
            logic.confirmed(&winner, now + Duration::from_millis(11));
            assert_eq!(logic.confirmed_total, 1);
            assert_eq!(logic.confirmed_forks, 1);
            assert_eq!(logic.sum_fork_time, Duration::from_millis(10));
            assert_eq!(logic.sum_nonfork_time, Duration::ZERO);
            assert_eq!(logic.delayed.len(), 0);
        }
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
}
