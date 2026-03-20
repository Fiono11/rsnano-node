use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rsnano_ledger::{Ledger, LedgerEvent};
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_utils::{
    BackpressureHandler, EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use super::{logic::BoundedBacklogLogic, walker::AccountWalker};
use crate::block_processing::{
    LedgerPipelineEvent, backlog_scan::UnconfirmedInfo, bounded_backlog::BoundedBacklogConfig,
};

/// Continuously rolls back unconfirmed blocks with the lowest priority
/// if the backlog exceeds the configured limit
/// This struct belongs to the application layer
pub struct BoundedBacklog {
    logic: NullableCondvarMutex<BoundedBacklogLogic>,
    ledger: Arc<Ledger>,
    should_throttle: AtomicBool,
}

impl BoundedBacklog {
    pub fn new(config: BoundedBacklogConfig, ledger: Arc<Ledger>) -> Self {
        let logic = NullableCondvarMutex::new(BoundedBacklogLogic::new(config));
        Self {
            logic,
            ledger,
            should_throttle: AtomicBool::new(false),
        }
    }

    pub fn new_null() -> Self {
        Self::new(Default::default(), Ledger::new_null().into())
    }

    fn set_cooldown(&self, cool_down: bool) {
        self.logic.lock().set_cooldown(cool_down);
        self.logic.notify_all();
    }

    pub fn stop(&self) {
        self.logic.lock().stop();
        self.logic.notify_all();
    }

    pub fn should_throttle_block_processor(&self) -> bool {
        self.should_throttle.load(Ordering::Relaxed)
    }

    pub(crate) fn run_loop(&self) {
        let mut logic = self.logic.lock();
        let mut targets = Vec::with_capacity(logic.rollback_batch_size());

        while !logic.stopped() {
            logic = self
                .logic
                .wait_timeout_while(logic, Duration::from_secs(1), |i| {
                    !i.stopped() && !i.rollback_needed()
                })
                .0;

            if logic.stopped() {
                return;
            }

            logic.set_bootstrap_weights_max_blocks(self.ledger.bootstrap_weights_max_blocks());
            logic.set_ledger_info(self.ledger.block_count(), self.ledger.confirmed_count());
            self.should_throttle
                .store(logic.should_throttle_block_processor(), Ordering::Relaxed);

            if !logic.rollback_needed() {
                continue;
            }

            logic.gather_targets(&mut targets);

            if !targets.is_empty() {
                let target_count = logic.rollback_target_count();
                drop(logic);

                self.ledger
                    .roll_back_batch(&*targets, target_count as usize);

                logic = self.logic.lock();
            }
        }
    }

    fn unconfirmed_accounts_found(&self, batch: &[UnconfirmedInfo]) {
        let mut walker = AccountWalker::new(&self.ledger);
        for info in batch {
            walker.walk_backwards(
                info.account_info.head,
                info.conf_info.frontier,
                |block, priority| self.logic.lock().insert(&block, priority),
            );
        }
    }
}

impl StatsSource for BoundedBacklog {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.logic.lock().collect_stats(result);
    }
}

impl ContainerInfoProvider for BoundedBacklog {
    fn container_info(&self) -> ContainerInfo {
        self.logic.lock().container_info()
    }
}

impl EventHandler<LedgerPipelineEvent> for BoundedBacklog {
    fn handle(&self, event: &LedgerPipelineEvent) {
        match event {
            LedgerPipelineEvent::Ledger(event) => match event {
                LedgerEvent::BlocksProcessed(results) => {
                    self.logic.lock().insert_processed(results);
                }
                LedgerEvent::BlocksConfirmed(confirmed) => {
                    self.logic
                        .lock()
                        .remove_batch(confirmed.iter().map(|i| i.0.hash()));
                }
                LedgerEvent::BlocksRolledBack(rolled_back) => {
                    self.logic.lock().remove_batch(rolled_back.hashes());
                }
            },
            LedgerPipelineEvent::UnconfirmedFound(unconfirmed) => {
                self.unconfirmed_accounts_found(unconfirmed);
            }
            _ => (),
        }
    }
}

impl BackpressureHandler for BoundedBacklog {
    fn cool_down(&self) {
        self.set_cooldown(true);
    }

    fn recovered(&self) {
        self.set_cooldown(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_ledger::{
        BlockSource, ProcessResult, RollbackResult, RollbackResults,
        test_helpers::UnsavedBlockLatticeBuilder,
    };
    use rsnano_nullable_condvar::NotifyEvent;
    use rsnano_types::{
        AccountInfo, Block, BlockHash, BlockPriority, ConfirmationHeightInfo, PrivateKey,
        QualifiedRoot, SavedBlock,
    };

    #[test]
    fn stop_immediately() {
        let logic = NullableCondvarMutex::null_builder(BoundedBacklogLogic::default())
            .wait(|l| l.stop())
            .finish();

        let ledger = Arc::new(Ledger::new_null());
        let backlog = create_backlog(logic, ledger);

        backlog.run_loop();

        // should not hang
    }

    #[test]
    fn set_current_backlog_size() {
        let logic = NullableCondvarMutex::null_builder(BoundedBacklogLogic::default())
            .wait(|_| {})
            .wait(|l| l.stop())
            .finish();

        let ledger = Arc::new(Ledger::new_null());
        let block = UnsavedBlockLatticeBuilder::new().genesis().send(1, 1);
        ledger.process_one(&block).unwrap();
        assert_eq!(ledger.backlog_size(), 1);

        let backlog = create_backlog(logic, ledger);
        backlog.run_loop();

        let logic = backlog.logic.lock();
        assert_eq!(logic.block_count(), 2);
        assert_eq!(logic.confirmed_count(), 1);
        assert_eq!(logic.rollback_iterations, 0);
    }

    #[test]
    fn set_bootstrap_weights_max_blocks() {
        let logic = NullableCondvarMutex::null_builder(BoundedBacklogLogic::default())
            .wait(|_| {})
            .wait(|l| l.stop())
            .finish();

        const MAX_BLOCKS: u64 = 123;
        let ledger = Arc::new(
            Ledger::new_null_builder()
                .bootstrap_weights_max_blocks(MAX_BLOCKS)
                .finish(),
        );
        let backlog = create_backlog(logic, ledger);

        backlog.run_loop();

        let logic = backlog.logic.lock();
        assert_eq!(logic.bootstrap_weights_max_blocks(), MAX_BLOCKS);
    }

    #[test]
    fn updates_throttle_flag() {
        let logic =
            NullableCondvarMutex::null_builder(BoundedBacklogLogic::new(BoundedBacklogConfig {
                max_backlog: 1,
                rollback_batch_size: 1,
            }))
            .wait(|_| {})
            .wait(|l| l.stop())
            .finish();

        let ledger = Arc::new(Ledger::new_null());
        let mut builder = UnsavedBlockLatticeBuilder::new();
        let block1 = builder.genesis().send(1, 1);
        let block2 = builder.genesis().send(1, 1);
        let block3 = builder.genesis().send(1, 1);
        ledger.process_one(&block1).unwrap();
        ledger.process_one(&block2).unwrap();
        ledger.process_one(&block3).unwrap();
        let backlog = create_backlog(logic, ledger);

        backlog.run_loop();

        assert!(backlog.should_throttle_block_processor());
    }

    #[test]
    fn gather_and_roll_back() {
        let config = BoundedBacklogConfig {
            max_backlog: 1,
            rollback_batch_size: 1,
        };

        let mut logic = BoundedBacklogLogic::new(config);

        let ledger = Arc::new(Ledger::new_null());
        let mut builder = UnsavedBlockLatticeBuilder::new();
        let block1 = builder.genesis().send(1, 1);
        let block2 = builder.genesis().send(1, 1);
        let block1 = ledger.process_one(&block1).unwrap();
        let block2 = ledger.process_one(&block2).unwrap();
        assert_eq!(ledger.backlog_size(), 2);

        logic.insert(&block1, BlockPriority::new_test_instance());
        logic.insert(&block2, BlockPriority::new_test_instance());

        let logic = NullableCondvarMutex::null_builder(logic)
            .wait(|_| {})
            .wait(|l| l.stop())
            .finish();

        let backlog = create_backlog(logic, ledger.clone());
        backlog.run_loop();

        let logic = backlog.logic.lock();
        assert_eq!(logic.backlog_size(), 2);
        assert_eq!(logic.block_count(), 3);
        assert_eq!(logic.rollback_iterations, 1);
        assert_ne!(ledger.backlog_size(), 2);
    }

    #[test]
    fn stop_sets_stopped_flag() {
        let backlog = BoundedBacklog::new_null();
        let tracker = backlog.logic.track_notifications();
        backlog.stop();
        assert!(backlog.logic.lock().stopped());
        assert_eq!(tracker.output(), vec![NotifyEvent::NotifyAll]);
    }

    #[test]
    fn cool_down_sets_flag() {
        let backlog = BoundedBacklog::new_null();
        let tracker = backlog.logic.track_notifications();
        backlog.cool_down();
        assert!(backlog.logic.lock().cool_down());
        assert_eq!(tracker.output(), vec![NotifyEvent::NotifyAll]);
    }

    #[test]
    fn recovered_sets_flag() {
        let backlog = BoundedBacklog::new_null();
        backlog.cool_down();
        let tracker = backlog.logic.track_notifications();

        backlog.recovered();

        assert!(!backlog.logic.lock().cool_down());
        assert_eq!(tracker.output(), vec![NotifyEvent::NotifyAll]);
    }

    #[test]
    fn collects_stats() {
        let backlog = BoundedBacklog::new_null();

        let mut expected = StatsCollection::new();
        backlog.logic.lock().collect_stats(&mut expected);

        let mut result = StatsCollection::new();
        backlog.collect_stats(&mut result);

        assert_eq!(result, expected);
    }

    #[test]
    fn collects_container_info() {
        let backlog = BoundedBacklog::new_null();

        let expected = backlog.logic.lock().container_info();
        let result = backlog.container_info();

        assert_eq!(result, expected);
    }

    /*
     * Tests for handling LedgerPipelineEvents
     */

    #[test]
    fn blocks_processed_inserts_blocks() {
        let backlog = BoundedBacklog::new_null();

        let saved_block = SavedBlock::new_test_instance();
        let hash = saved_block.hash();
        let result = ProcessResult {
            block: Block::new_test_instance(),
            source: BlockSource::Live,
            status: Ok(()),
            saved_block: Some(saved_block),
            priority: BlockPriority::new_test_instance(),
        };

        backlog.handle(&LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(
            vec![result],
        )));

        assert!(backlog.logic.lock().contains(&hash));
    }

    #[test]
    fn blocks_confirmed_removes_blocks() {
        let backlog = BoundedBacklog::new_null();

        let saved_block = SavedBlock::new_test_instance();
        let hash = saved_block.hash();
        backlog
            .logic
            .lock()
            .insert(&saved_block, BlockPriority::new_test_instance());

        backlog.handle(&LedgerPipelineEvent::Ledger(LedgerEvent::BlocksConfirmed(
            vec![(saved_block, BlockHash::ZERO)],
        )));

        assert!(!backlog.logic.lock().contains(&hash));
    }

    #[test]
    fn blocks_rolled_back_removes_blocks() {
        let backlog = BoundedBacklog::new_null();

        let saved_block = SavedBlock::new_test_instance();
        let hash = saved_block.hash();
        backlog
            .logic
            .lock()
            .insert(&saved_block, BlockPriority::new_test_instance());

        let mut results = RollbackResults::new();
        results.push(RollbackResult {
            target_hash: hash,
            target_root: QualifiedRoot::new_test_instance(),
            rolled_back: vec![saved_block],
            error: None,
        });

        backlog.handle(&LedgerPipelineEvent::Ledger(LedgerEvent::BlocksRolledBack(
            results,
        )));

        assert!(!backlog.logic.lock().contains(&hash));
    }

    #[test]
    fn unconfirmed_found_inserts_blocks() {
        let logic = NullableCondvarMutex::new(BoundedBacklogLogic::default());
        let ledger = Arc::new(Ledger::new_null());

        let account_key = PrivateKey::from(123);
        let mut builder = UnsavedBlockLatticeBuilder::new();
        let genesis_send = builder.genesis().send(&account_key, 1000);
        let open = builder.account(&account_key).receive(&genesis_send);
        ledger.process_one(&genesis_send).unwrap();
        let saved_open = ledger.process_one(&open).unwrap();

        let backlog = create_backlog(logic, ledger);

        let info = UnconfirmedInfo {
            account: saved_open.account(),
            account_info: AccountInfo {
                head: saved_open.hash(),
                ..Default::default()
            },
            conf_info: ConfirmationHeightInfo::default(),
        };

        backlog.handle(&LedgerPipelineEvent::UnconfirmedFound(vec![info]));

        assert!(backlog.logic.lock().contains(&saved_open.hash()));
    }

    /*
     * Test helpers
     */

    fn create_backlog(
        logic: NullableCondvarMutex<BoundedBacklogLogic>,
        ledger: Arc<Ledger>,
    ) -> BoundedBacklog {
        BoundedBacklog {
            logic,
            ledger,
            should_throttle: AtomicBool::new(false),
        }
    }
}
