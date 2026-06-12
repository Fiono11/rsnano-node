use crate::{block_processing::LedgerPipelineEvent, ledger_snapshots::LedgerSnapshots};
use rsnano_ledger::{AnySet, LedgerEvent, LedgerSet};
use rsnano_ledger::{BlockError, Ledger};
use rsnano_messages::{RaiElectionId, RaiSlot};
use rsnano_types::Block;
use rsnano_utils::EventHandlerMut;
use std::sync::Arc;

pub(crate) struct ForkDetector {
    ledger: Arc<Ledger>,
    ledger_snapshots: Arc<LedgerSnapshots>,
}

impl ForkDetector {
    pub(crate) fn new(ledger: Arc<Ledger>, ledger_snapshots: Arc<LedgerSnapshots>) -> Self {
        Self {
            ledger,
            ledger_snapshots,
        }
    }
}

impl EventHandlerMut<LedgerPipelineEvent> for ForkDetector {
    fn handle(&mut self, event: &LedgerPipelineEvent) {
        if let LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(results)) = event {
            for result in results {
                if result.status == Err(BlockError::Fork) {
                    let root = result.block.qualified_root();
                    tracing::debug!("Fork detected: {:?}", root);

                    self.ledger
                        .mark_fork(&root, self.ledger_snapshots.rai().current_open_epoch());

                    if let Some(election) = self.rai_election_for(&result.block) {
                        let existing_successor =
                            self.ledger.any().block_successor_by_qualified_root(&root);
                        self.ledger_snapshots.handle_rai_block_conflict(
                            election,
                            result.block.hash(),
                            existing_successor,
                        );
                    }
                }
            }
        }
    }
}

impl ForkDetector {
    fn rai_election_for(&self, block: &Block) -> Option<RaiElectionId> {
        let any = self.ledger.any();
        let account = block.account_field().or_else(|| {
            (!block.previous().is_zero())
                .then(|| {
                    any.get_block(&block.previous())
                        .map(|previous| previous.account())
                })
                .flatten()
        })?;

        let slot_index = if block.previous().is_zero() {
            1
        } else if let Some(previous) = any.get_block(&block.previous()) {
            previous.height() + 1
        } else {
            any.get_account(&account)?.block_count + 1
        };

        Some(
            self.ledger_snapshots
                .rai()
                .election_id(RaiSlot::new(account, slot_index)),
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        block_processing::LedgerPipelineEvent,
        consensus::{AecInsertRequest, AecService, election::ElectionBehavior},
        ledger_snapshots::{LedgerSnapshots, fork_detector::ForkDetector},
    };
    use rsnano_ledger::{BlockError, Ledger};
    use rsnano_ledger::{BlockSource, LedgerEvent, ProcessResult};
    use rsnano_nullable_clock::Timestamp;
    use rsnano_types::{Block, BlockPriority, SavedBlock, TestBlockBuilder};
    use rsnano_utils::EventHandlerMut;
    use std::sync::Arc;

    #[test]
    fn marks_a_forked_block_in_the_ledger() {
        let ledger = Arc::new(Ledger::new_null());
        let ledger_snapshots = LedgerSnapshots::new_null();
        let snapshot_number = ledger_snapshots.get_current_snapshot_number();
        let mut fork_detector = ForkDetector::new(ledger.clone(), ledger_snapshots.into());
        let block = Block::new_test_instance();
        let root = block.qualified_root();

        let processed_results = ProcessResult {
            block,
            source: BlockSource::Live,
            status: Err(BlockError::Fork),
            saved_block: None,
            priority: BlockPriority::new_test_instance(),
        };

        fork_detector.handle(&LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(
            vec![processed_results],
        )));

        assert_eq!(
            ledger
                .store
                .forks
                .get(&ledger.store.env.begin_read(), &root),
            Some(snapshot_number)
        );
    }

    #[test]
    fn can_mark_multiple_forks_in_one_go() {
        let ledger = Arc::new(Ledger::new_null());
        let ledger_snapshots = LedgerSnapshots::new_null();
        let snapshot_number = ledger_snapshots.get_current_snapshot_number();
        let mut fork_detector = ForkDetector::new(ledger.clone(), ledger_snapshots.into());
        let block1 = Block::new_test_instance_with_key(1);
        let block2 = Block::new_test_instance_with_key(2);
        let root1 = block1.qualified_root();
        let root2 = block2.qualified_root();

        let processed_result1 = ProcessResult {
            block: block1,
            source: BlockSource::Live,
            status: Err(BlockError::Fork),
            saved_block: None,
            priority: BlockPriority::new_test_instance(),
        };

        let processed_result2 = ProcessResult {
            block: block2,
            source: BlockSource::Live,
            status: Err(BlockError::Fork),
            saved_block: None,
            priority: BlockPriority::new_test_instance(),
        };

        fork_detector.handle(&LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(
            vec![processed_result1, processed_result2],
        )));

        assert_eq!(
            ledger
                .store
                .forks
                .get(&ledger.store.env.begin_read(), &root1),
            Some(snapshot_number)
        );

        assert_eq!(
            ledger
                .store
                .forks
                .get(&ledger.store.env.begin_read(), &root2),
            Some(snapshot_number)
        );
    }

    #[test]
    fn ignores_blocks_without_fork() {
        let ledger = Arc::new(Ledger::new_null());
        let ledger_snapshots = LedgerSnapshots::new_null();
        let mut fork_detector = ForkDetector::new(ledger.clone(), ledger_snapshots.into());
        let block = Block::new_test_instance();
        let root = block.qualified_root();

        let processed_results = ProcessResult {
            block,
            source: BlockSource::Live,
            status: Err(BlockError::GapPrevious),
            saved_block: None,
            priority: BlockPriority::new_test_instance(),
        };

        fork_detector.handle(&LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(
            vec![processed_results],
        )));

        assert_eq!(
            ledger
                .store
                .forks
                .get(&ledger.store.env.begin_read(), &root),
            None
        );
    }

    #[test]
    fn feeds_forked_block_to_rai() {
        let previous = SavedBlock::new_test_instance_with_key(1);
        let existing_successor = TestBlockBuilder::state()
            .account(previous.account())
            .previous(previous.hash())
            .link(10)
            .build_saved();
        let fork = TestBlockBuilder::state()
            .account(previous.account())
            .previous(previous.hash())
            .link(11)
            .build();

        let ledger = Arc::new(Ledger::new_null_builder().block(&previous).finish());
        let mut tx = ledger.store.begin_write();
        ledger
            .store
            .successors
            .put(&mut tx, &previous.hash(), &existing_successor.hash());
        tx.commit();

        let ledger_snapshots = Arc::new(LedgerSnapshots::new_null());
        ledger_snapshots.start_ledger_snapshot();
        let snapshot_number = ledger_snapshots.rai().current_open_epoch();
        let mut fork_detector = ForkDetector::new(ledger.clone(), ledger_snapshots.clone());

        let processed_results = ProcessResult {
            block: fork.clone(),
            source: BlockSource::Live,
            status: Err(BlockError::Fork),
            saved_block: None,
            priority: BlockPriority::new_test_instance(),
        };

        fork_detector.handle(&LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(
            vec![processed_results],
        )));

        let election = fork_detector.rai_election_for(&fork).unwrap();
        assert_eq!(election.epoch, snapshot_number);
        assert_eq!(election.epoch, 1);
        assert_eq!(election.slot.account, previous.account());
        assert_eq!(election.slot.index, previous.height() + 1);

        let election_state = ledger_snapshots.rai().election(&election).unwrap();
        assert!(
            election_state
                .proposals
                .contains(&existing_successor.hash())
        );
        assert!(election_state.proposals.contains(&fork.hash()));
    }

    #[test]
    fn does_not_discard_active_election_when_fork_is_detected() {
        let block = SavedBlock::new_test_instance();
        let aec = Arc::new(AecService::new_null());
        let request = AecInsertRequest {
            block: block.clone(),
            behavior: ElectionBehavior::Priority,
            priority: BlockPriority::new_test_instance(),
        };

        aec.insert(request, Timestamp::new_test_instance()).unwrap();

        let ledger = Arc::new(Ledger::new_null());
        let ledger_snapshots = LedgerSnapshots::new_null();
        let mut fork_detector = ForkDetector::new(ledger.clone(), ledger_snapshots.into());

        let processed_results = ProcessResult {
            block: block.into(),
            source: BlockSource::Live,
            status: Err(BlockError::Fork),
            saved_block: None,
            priority: BlockPriority::new_test_instance(),
        };

        fork_detector.handle(&LedgerPipelineEvent::Ledger(LedgerEvent::BlocksProcessed(
            vec![processed_results],
        )));

        assert_eq!(aec.len(), 1);
    }
}
