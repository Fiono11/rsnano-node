use std::{
    sync::{Arc, MutexGuard},
    time::Duration,
};

use tracing::info;

use rsnano_ledger::{Ledger, LedgerEvent};
use rsnano_nullable_condvar::NullableCondvarMutex;
use rsnano_types::BlockHash;
use rsnano_utils::{
    EventHandler,
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
}

impl BoundedBacklog {
    pub fn new(config: BoundedBacklogConfig, ledger: Arc<Ledger>) -> Self {
        let logic = NullableCondvarMutex::new(BoundedBacklogLogic::new(config));
        Self { logic, ledger }
    }

    pub fn new_null() -> Self {
        Self::new(Default::default(), Ledger::new_null().into())
    }

    pub fn set_cooldown(&self, cool_down: bool) {
        self.logic.lock().set_cool_down(cool_down);
        self.logic.notify_all();
    }

    pub fn stop(&self) {
        self.logic.lock().stop();
        self.logic.notify_all();
    }

    pub(crate) fn run_loop(&self) {
        let mut logic = self.logic.lock();
        info!(
            "Bounded backlog enabled: max backlog={}, batch_size={}",
            logic.max_backlog(),
            logic.rollback_batch_size(),
        );

        let mut targets = Vec::with_capacity(logic.rollback_batch_size());

        while !logic.stopped() {
            logic = self
                .logic
                .wait_timeout_while(logic, Duration::from_secs(1), |i| {
                    !i.stopped() && !i.rollback_needed()
                })
                .0;

            logic = self.tick(logic, &mut targets);
        }
    }

    fn tick<'a>(
        &'a self,
        mut logic: MutexGuard<'a, BoundedBacklogLogic>,
        targets: &mut Vec<BlockHash>,
    ) -> MutexGuard<'a, BoundedBacklogLogic> {
        if logic.stopped() {
            return logic;
        }

        logic.set_current_backlog_size(self.ledger.backlog_size());

        if !logic.rollback_needed() {
            return logic;
        }

        logic.gather_targets(targets);

        if !targets.is_empty() {
            let target_count = logic.rollback_target_count();
            drop(logic);

            self.ledger
                .roll_back_batch(&*targets, target_count as usize);

            logic = self.logic.lock();
        }

        logic
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

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_ledger::{BlockSource, ProcessResult, test_helpers::UnsavedBlockLatticeBuilder};
    use rsnano_nullable_condvar::NotifyEvent;
    use rsnano_types::{Block, BlockPriority, SavedBlock};
    use tracing_test::traced_test;

    #[test]
    #[traced_test]
    fn run_loop_logs_current_configuration() {
        let backlog = BoundedBacklog::new_null();
        backlog.stop();
        backlog.run_loop();
        assert!(logs_contain(
            "Bounded backlog enabled: max backlog=100000, batch_size=32"
        ));
    }

    #[test]
    fn stop_immediately() {
        let logic = NullableCondvarMutex::null_builder(BoundedBacklogLogic::default())
            .wait(|l| l.stop())
            .finish();

        let ledger = Arc::new(Ledger::new_null());
        let backlog = BoundedBacklog { logic, ledger };

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

        let backlog = BoundedBacklog { logic, ledger };
        backlog.run_loop();

        let logic = backlog.logic.lock();
        assert_eq!(logic.current_backlog_size(), 1);
        assert_eq!(logic.gather_called, 0);
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

        let backlog = BoundedBacklog {
            logic,
            ledger: ledger.clone(),
        };
        backlog.run_loop();

        let logic = backlog.logic.lock();
        assert_eq!(logic.current_backlog_size(), 2);
        assert_eq!(logic.gather_called, 1);
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
    fn set_cooldown_sets_flag() {
        let backlog = BoundedBacklog::new_null();
        let tracker = backlog.logic.track_notifications();
        backlog.set_cooldown(true);
        assert!(backlog.logic.lock().cool_down());
        assert_eq!(tracker.output(), vec![NotifyEvent::NotifyAll]);
    }

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
}
