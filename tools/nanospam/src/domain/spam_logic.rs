use crate::domain::{
    AccountMap, BlockFactory, BlockResult, DelayedBlocks, Forks, RateSpec, SpamStrategy,
    high_prio_tracker::HighPrioTracker,
};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Block, BlockHash, PublicKey};
use std::time::Duration;

pub(crate) struct SpamSpec {
    pub(crate) spam_strategy: SpamStrategy,
    pub(crate) max_blocks: usize,
    pub(crate) rate: RateSpec,
    pub(crate) fork_probability: f64,
    pub(crate) track_confirmations: bool,
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum NextBlockResult {
    Block(Forks),
    RateLimited(Duration),
    NoReadyAccount,
}

/// Token-bucket pacing for the benchmark producer. Unlike the generic network
/// limiter, this reports the exact time at which one more token can exist, so
/// the dedicated producer thread can park instead of spinning between blocks.
struct SpamRateLimiter {
    last_refill: Option<Timestamp>,
    current_size: usize,
    max_token_count: usize,
    refill_rate: usize,
    unlimited: bool,
}

impl SpamRateLimiter {
    fn new(refill_rate: usize) -> Self {
        if refill_rate == 0 {
            Self {
                last_refill: None,
                current_size: 0,
                max_token_count: 0,
                refill_rate: 0,
                unlimited: true,
            }
        } else {
            Self {
                last_refill: None,
                current_size: 1,
                max_token_count: 1,
                refill_rate,
                unlimited: false,
            }
        }
    }

    fn try_consume(&mut self, now: Timestamp) -> Result<(), Duration> {
        if self.unlimited {
            return Ok(());
        }

        self.refill(now);
        if self.current_size > 0 {
            self.current_size -= 1;
            Ok(())
        } else {
            Err(self.retry_after(now))
        }
    }

    fn set_limit(&mut self, new_limit: usize) {
        if new_limit == 0 {
            self.unlimited = true;
            return;
        }
        self.unlimited = false;
        self.max_token_count = new_limit;
        self.refill_rate = new_limit;
    }

    fn refill(&mut self, now: Timestamp) {
        let Some(last_refill) = self.last_refill else {
            self.last_refill = Some(now);
            return;
        };
        let tokens_to_add = last_refill
            .elapsed(now)
            .as_nanos()
            .saturating_mul(self.refill_rate as u128)
            / 1_000_000_000;
        let tokens_to_add = usize::try_from(tokens_to_add).unwrap_or(usize::MAX);
        if tokens_to_add > 0 {
            self.current_size = self
                .current_size
                .saturating_add(tokens_to_add)
                .min(self.max_token_count);
            // Match token-bucket semantics by discarding a fractional token
            // whenever at least one complete token was added.
            self.last_refill = Some(now);
        }
    }

    fn retry_after(&self, now: Timestamp) -> Duration {
        let nanos_per_token =
            (1_000_000_000_u128 + self.refill_rate as u128 - 1) / self.refill_rate as u128;
        let elapsed = self
            .last_refill
            .map(|last_refill| last_refill.elapsed(now).as_nanos())
            .unwrap_or_default();
        let remaining = nanos_per_token.saturating_sub(elapsed).max(1);
        Duration::from_nanos(u64::try_from(remaining).unwrap_or(u64::MAX))
    }
}

pub(crate) struct SpamLogic {
    pub(crate) delayed: DelayedBlocks,
    pub(crate) high_prio_tracker: HighPrioTracker,
    pub(crate) block_factory: BlockFactory,
    pub(crate) current_bps: usize,
    bps_limiter: SpamRateLimiter,
    next_block: Option<Forks>,
    bps_start: Option<Timestamp>,
    spec: SpamSpec,
    pub(crate) confirmed_total: usize,
    published_total: usize,
    pub(crate) confirmed_recent: usize,
    pub(crate) sum_conf_time_recent: Duration,
    websocket_conf_time_total: Duration,
    websocket_confirmed_total: usize,
    pub(crate) cps_measure_start: Option<Timestamp>,
}

impl SpamLogic {
    pub(crate) fn new(
        account_map: AccountMap,
        spec: SpamSpec,
        live_representatives: Vec<PublicKey>,
    ) -> Self {
        Self {
            delayed: Default::default(),
            high_prio_tracker: Default::default(),
            block_factory: BlockFactory::new_with_live_representatives(
                account_map,
                spec.max_blocks,
                spec.spam_strategy,
                live_representatives,
            ),
            current_bps: spec.rate.initial_bps,
            bps_limiter: SpamRateLimiter::new(spec.rate.initial_bps),
            next_block: None,
            bps_start: None,
            spec,
            confirmed_total: 0,
            published_total: 0,
            confirmed_recent: 0,
            sum_conf_time_recent: Duration::ZERO,
            websocket_conf_time_total: Duration::ZERO,
            websocket_confirmed_total: 0,
            cps_measure_start: None,
        }
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.block_factory.max_blocks() > 0
            && self.confirmed_total >= self.block_factory.max_blocks()
    }

    pub(crate) fn fork_propability(&self) -> f64 {
        self.spec.fork_probability
    }

    pub(crate) fn next_block(&mut self, is_fork: bool, now: Timestamp) -> Option<NextBlockResult> {
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
                Some(BlockResult::Waiting) => return Some(NextBlockResult::NoReadyAccount),
                None => unreachable!(),
            }
        }

        if let Err(retry_after) = self.bps_limiter.try_consume(now) {
            return Some(NextBlockResult::RateLimited(retry_after));
        }

        let next = self.next_block.take().unwrap();
        self.delayed.insert(next.block.clone()); // TODO: handle forks!

        if self.bps_start.unwrap().elapsed(now) >= self.spec.rate.interval {
            self.current_bps += self.spec.rate.increment;
            self.bps_limiter.set_limit(self.current_bps);
            self.bps_start = Some(now);
        }

        Some(NextBlockResult::Block(next))
    }

    pub(crate) fn next_delayed(&mut self, now: Timestamp) -> Option<Block> {
        self.delayed.next(now)
    }

    pub(crate) fn published(&mut self, hash: &BlockHash, now: Timestamp) -> bool {
        if self.delayed.published(hash, now) {
            self.published_total += 1;
        }

        if !self.spec.track_confirmations {
            self.delayed.confirmed(hash, now);
            self.block_factory.confirm(hash);
            self.confirmed_total += 1;
        }
        self.high_prio_tracker.published(hash, now)
    }

    pub(crate) fn published_total(&self) -> usize {
        self.published_total
    }

    fn confirm(
        &mut self,
        block_hash: &BlockHash,
        timestamp: Timestamp,
    ) -> (Option<Duration>, Option<Duration>) {
        let mut confirmation_time = None;
        if self.spec.track_confirmations {
            let conf_time = self.delayed.confirmed(block_hash, timestamp);

            if let Some(conf_time) = conf_time {
                if self.cps_measure_start.is_none() {
                    self.cps_measure_start = Some(timestamp);
                }
                self.confirmed_recent += 1;
                self.confirmed_total += 1;
                self.sum_conf_time_recent += conf_time;
                confirmation_time = Some(conf_time);
            }
            self.block_factory.confirm(block_hash);
        }

        (
            self.high_prio_tracker.confirmed(block_hash, timestamp),
            confirmation_time,
        )
    }

    pub(crate) fn confirmed_from_websocket(
        &mut self,
        block_hash: &BlockHash,
        timestamp: Timestamp,
    ) -> Option<Duration> {
        let (high_prio_conf_time, confirmation_time) = self.confirm(block_hash, timestamp);
        if let Some(confirmation_time) = confirmation_time {
            self.websocket_conf_time_total += confirmation_time;
            self.websocket_confirmed_total += 1;
        }

        high_prio_conf_time
    }

    pub(crate) fn average_websocket_confirmation_time(&self) -> Option<Duration> {
        (self.websocket_confirmed_total > 0)
            .then(|| self.websocket_conf_time_total / self.websocket_confirmed_total as u32)
    }

    pub(crate) fn websocket_confirmation_samples(&self) -> usize {
        self.websocket_confirmed_total
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
    use rsnano_types::Amount;

    #[test]
    fn distinguishes_rate_limiting_from_account_readiness() {
        let mut account_map = AccountMap::default();
        account_map.fill(2);
        for (index, account) in account_map.accounts().clone().into_iter().enumerate() {
            account_map.set_account_state(
                account,
                Amount::nano(1),
                BlockHash::from(index as u64 + 1),
            );
        }
        let mut logic = SpamLogic::new(
            account_map,
            SpamSpec {
                spam_strategy: SpamStrategy::SendReceive,
                max_blocks: 2,
                rate: RateSpec::new(1_450),
                fork_probability: 0.0,
                track_confirmations: true,
            },
            Vec::new(),
        );
        let now = Timestamp::new_test_instance();

        assert!(matches!(
            logic.next_block(false, now),
            Some(NextBlockResult::Block(_))
        ));
        assert!(matches!(
            logic.next_block(false, now),
            Some(NextBlockResult::RateLimited(_))
        ));

        // The rate-limited block is retained inside SpamLogic, so account
        // readiness is not consulted again until that block is emitted.
        assert!(matches!(
            logic.next_block(false, now),
            Some(NextBlockResult::RateLimited(_))
        ));
    }

    #[test]
    fn rate_limit_reports_the_exact_remaining_token_interval() {
        let mut account_map = AccountMap::default();
        account_map.fill(2);
        for (index, account) in account_map.accounts().clone().into_iter().enumerate() {
            account_map.set_account_state(
                account,
                Amount::nano(1),
                BlockHash::from(index as u64 + 1),
            );
        }
        let mut logic = SpamLogic::new(
            account_map,
            SpamSpec {
                spam_strategy: SpamStrategy::SendReceive,
                max_blocks: 2,
                rate: RateSpec::new(1_000),
                fork_probability: 0.0,
                track_confirmations: true,
            },
            Vec::new(),
        );
        let now = Timestamp::new_test_instance();

        assert!(matches!(
            logic.next_block(false, now),
            Some(NextBlockResult::Block(_))
        ));
        assert!(matches!(
            logic.next_block(false, now),
            Some(NextBlockResult::RateLimited(wait))
                if wait == Duration::from_millis(1)
        ));
        assert!(matches!(
            logic.next_block(false, now + Duration::from_micros(600)),
            Some(NextBlockResult::RateLimited(wait))
                if wait == Duration::from_micros(400)
        ));
        assert!(matches!(
            logic.next_block(false, now + Duration::from_millis(1)),
            Some(NextBlockResult::Block(_))
        ));
    }

    #[test]
    fn rate_limit_retains_catch_up_tokens_after_a_scheduling_gap() {
        let mut account_map = AccountMap::default();
        account_map.fill(12);
        for (index, account) in account_map.accounts().clone().into_iter().enumerate() {
            account_map.set_account_state(
                account,
                Amount::nano(1),
                BlockHash::from(index as u64 + 1),
            );
        }
        let mut logic = SpamLogic::new(
            account_map,
            SpamSpec {
                spam_strategy: SpamStrategy::SendReceive,
                max_blocks: 12,
                rate: RateSpec::new(1_000),
                fork_probability: 0.0,
                track_confirmations: true,
            },
            Vec::new(),
        );
        let now = Timestamp::new_test_instance();

        assert!(matches!(
            logic.next_block(false, now),
            Some(NextBlockResult::Block(_))
        ));
        let after_gap = now + Duration::from_millis(10);
        for _ in 0..10 {
            assert!(matches!(
                logic.next_block(false, after_gap),
                Some(NextBlockResult::Block(_))
            ));
        }
        assert!(matches!(
            logic.next_block(false, after_gap),
            Some(NextBlockResult::RateLimited(wait))
                if wait == Duration::from_millis(1)
        ));
    }

    #[test]
    fn reports_when_no_account_is_ready() {
        let mut account_map = AccountMap::default();
        account_map.fill(1);
        let initial = account_map.initial_key().account();
        account_map.set_account_state(initial, Amount::nano(1), BlockHash::from(1));
        let mut logic = SpamLogic::new(
            account_map,
            SpamSpec {
                spam_strategy: SpamStrategy::SendReceive,
                max_blocks: 2,
                rate: RateSpec::new(1_450),
                fork_probability: 0.0,
                track_confirmations: true,
            },
            Vec::new(),
        );
        let now = Timestamp::new_test_instance();

        assert!(matches!(
            logic.next_block(false, now),
            Some(NextBlockResult::Block(_))
        ));
        assert!(matches!(
            logic.next_block(false, now),
            Some(NextBlockResult::NoReadyAccount)
        ));
    }

    #[test]
    fn rate_limited_final_block_is_emitted_after_factory_reaches_max() {
        let mut account_map = AccountMap::default();
        account_map.fill(2);
        let initial = account_map.initial_key().account();
        account_map.set_account_state(initial, Amount::nano(1), BlockHash::from(1));
        let mut logic = SpamLogic::new(
            account_map,
            SpamSpec {
                spam_strategy: SpamStrategy::SendReceive,
                max_blocks: 2,
                rate: RateSpec::new(1),
                fork_probability: 0.0,
                track_confirmations: true,
            },
            Vec::new(),
        );
        let now = Timestamp::new_test_instance();
        let NextBlockResult::Block(first) = logic.next_block(false, now).unwrap() else {
            panic!("first block should be emitted")
        };
        logic.published(&first.block.hash(), now);
        logic.confirmed_from_websocket(&first.block.hash(), now);
        assert!(matches!(
            logic.next_block(false, now),
            Some(NextBlockResult::RateLimited(_))
        ));
        assert_eq!(logic.block_factory.created(), 2);

        assert!(matches!(
            logic.next_block(false, now + Duration::from_secs(1)),
            Some(NextBlockResult::Block(_))
        ));
        assert!(
            logic
                .next_block(false, now + Duration::from_secs(1))
                .is_none()
        );
    }

    #[test]
    fn websocket_confirmation_average_excludes_other_confirmation_paths() {
        let mut account_map = AccountMap::default();
        account_map.fill(3);
        let initial = account_map.initial_key().account();
        account_map.set_account_state(initial, Amount::nano(1), BlockHash::from(1));
        let mut logic = SpamLogic::new(
            account_map,
            SpamSpec {
                spam_strategy: SpamStrategy::SendReceive,
                max_blocks: 2,
                rate: RateSpec::new(2),
                fork_probability: 0.0,
                track_confirmations: true,
            },
            Vec::new(),
        );
        let now = Timestamp::new_test_instance();

        let NextBlockResult::Block(first) = logic.next_block(false, now).unwrap() else {
            panic!("first block should be emitted")
        };
        logic.published(&first.block.hash(), now);
        logic.confirmed_from_websocket(&first.block.hash(), now + Duration::from_millis(200));

        let second_publish = now + Duration::from_millis(500);
        let NextBlockResult::Block(second) = logic.next_block(false, second_publish).unwrap()
        else {
            panic!("second block should be emitted")
        };
        logic.published(&second.block.hash(), second_publish);
        logic.confirm(
            &second.block.hash(),
            second_publish + Duration::from_secs(5),
        );

        assert_eq!(logic.websocket_confirmation_samples(), 1);
        assert_eq!(
            logic.average_websocket_confirmation_time(),
            Some(Duration::from_millis(200))
        );
    }
}
