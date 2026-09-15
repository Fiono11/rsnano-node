use rsnano_types::{Account, Block, PendingKey, SavedBlock, UnixMillisTimestamp};

use super::BlockValidator;
use crate::{AnySet, LedgerConstants};

pub(crate) struct BlockValidatorFactory<'a> {
    any: &'a dyn AnySet,
    constants: &'a LedgerConstants,
    block: &'a Block,
}

impl<'a> BlockValidatorFactory<'a> {
    pub(crate) fn new(
        any: &'a dyn AnySet,
        constants: &'a LedgerConstants,
        block: &'a Block,
    ) -> Self {
        Self {
            any,
            constants,
            block,
        }
    }

    pub(crate) fn create_validator(&self) -> BlockValidator<'a> {
        let previous_block = self.load_previous_block();
        let account = self.get_account(&previous_block);
        let account = account.unwrap_or_default();
        let source_block = self.block.source_or_link();

        let pending_receive_info = if source_block.is_zero() {
            None
        } else {
            self.any
                .get_pending(&PendingKey::new(account, source_block))
        };

        let existing_block = self.any.get_block(&self.block.hash());

        let mut validator = BlockValidator {
            block: self.block,
            epochs: &self.constants.epochs,
            work: &self.constants.work,
            account,
            existing_block,
            old_account_info: self.any.get_account(&account),
            pending_receive_info,
            any_pending_exists: false,
            source_block_exists: false,
            previous_block,
            now: UnixMillisTimestamp::now(),
        };

        // These reads only support receive and epoch-open rules. Use the validator's
        // classification so malformed blocks retain the same validation behavior.
        if validator.is_receive() && !source_block.is_zero() {
            validator.source_block_exists = self.any.block_exists(&source_block);
        }
        if self.block.is_open() && validator.is_epoch_block() {
            validator.any_pending_exists = self.any.receivable_exists(account);
        }

        validator
    }

    fn get_account(&self, previous: &Option<SavedBlock>) -> Option<Account> {
        match self.block.account_field() {
            Some(account) => Some(account),
            None => previous.as_ref().map(|p| p.account()),
        }
    }

    fn load_previous_block(&self) -> Option<SavedBlock> {
        if !self.block.previous().is_zero() {
            self.any.get_block(&self.block.previous())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, time::Duration};

    use crate::{
        BlockError, Ledger, LedgerSet, block_insertion::BlockInsertInstructions,
        ledger_sets::BorrowingAnySet,
    };

    use super::*;
    use rsnano_nullable_lmdb::{LmdbDatabase, RoCursor, Transaction};
    use rsnano_store_lmdb::BLOCK_INDEX_DATABASE;
    use rsnano_types::{
        AccountInfo, Amount, BlockHash, Epoch, Link, PendingInfo, SavedAccountChain,
        TestBlockBuilder, epoch_v1_link,
    };

    #[test]
    fn block_for_unknown_account() {
        let block = TestBlockBuilder::state().build();
        let ledger = Ledger::new_null_builder().finish();
        let any = ledger.any();
        let validator =
            BlockValidatorFactory::new(&any, &ledger.constants, &block).create_validator();

        assert_eq!(validator.block.hash(), block.hash());
        assert_eq!(validator.epochs, &ledger.constants.epochs);
        assert_eq!(validator.account, block.account_field().unwrap());
        assert!(validator.existing_block.is_none());
        assert_eq!(validator.old_account_info, None);
        assert_eq!(validator.pending_receive_info, None);
        assert_eq!(validator.any_pending_exists, false);
        assert_eq!(validator.source_block_exists, false);
        assert_eq!(validator.previous_block, None);
        assert!(validator.now >= UnixMillisTimestamp::now());
    }

    #[test]
    fn get_account_from_previous_block() {
        let previous = TestBlockBuilder::legacy_send().build_saved();
        let block = TestBlockBuilder::legacy_send()
            .previous(previous.hash())
            .build();
        let ledger = Ledger::new_null_builder().block(&previous).finish();
        let any = ledger.any();
        let validator =
            BlockValidatorFactory::new(&any, &ledger.constants, &block).create_validator();

        assert_eq!(validator.account, previous.account());
    }

    #[test]
    fn block_exists() {
        let block = TestBlockBuilder::state().build_saved();
        let ledger = Ledger::new_null_builder().block(&block).finish();
        let any = ledger.any();
        let validator =
            BlockValidatorFactory::new(&any, &ledger.constants, &block).create_validator();
        assert!(validator.existing_block.is_some());
    }

    #[test]
    fn account_info() {
        let block = TestBlockBuilder::state().build();
        let account_info = AccountInfo::new_test_instance();
        let ledger = Ledger::new_null_builder()
            .account_info(&block.account_field().unwrap(), &account_info)
            .finish();
        let any = ledger.any();
        let validator =
            BlockValidatorFactory::new(&any, &ledger.constants, &block).create_validator();
        assert_eq!(validator.old_account_info, Some(account_info));
    }

    #[test]
    fn pending_receive_info_for_state_block() {
        let block = TestBlockBuilder::state().link(Link::from(42)).build();
        let pending_info = PendingInfo::new_test_instance();
        let ledger = Ledger::new_null_builder()
            .pending(
                &PendingKey::new(block.account_field().unwrap(), BlockHash::from(42)),
                &pending_info,
            )
            .finish();
        let any = ledger.any();
        let validator =
            BlockValidatorFactory::new(&any, &ledger.constants, &block).create_validator();
        assert_eq!(validator.pending_receive_info, Some(pending_info));
    }

    #[test]
    fn pending_receive_info_for_legacy_receive() {
        let previous = TestBlockBuilder::legacy_open().build_saved();
        let account = previous.account();
        let block = TestBlockBuilder::legacy_receive()
            .previous(previous.hash())
            .source(BlockHash::from(42))
            .build();
        let pending_info = PendingInfo::new_test_instance();
        let ledger = Ledger::new_null_builder()
            .block(&previous)
            .pending(
                &PendingKey::new(account, BlockHash::from(42)),
                &pending_info,
            )
            .finish();
        let any = ledger.any();
        let validator =
            BlockValidatorFactory::new(&any, &ledger.constants, &block).create_validator();
        assert_eq!(validator.pending_receive_info, Some(pending_info));
    }

    #[test]
    fn any_pending_exists() {
        let block = SavedAccountChain::new().new_epoch1_open_block().build();
        let pending_info = PendingInfo::new_test_instance();
        let ledger = Ledger::new_null_builder()
            .pending(
                &PendingKey::new(block.account_field().unwrap(), BlockHash::from(42)),
                &pending_info,
            )
            .finish();
        let any = ledger.any();
        let validator =
            BlockValidatorFactory::new(&any, &ledger.constants, &block).create_validator();
        assert_eq!(validator.any_pending_exists, true);
    }

    #[test]
    fn source_block_exists() {
        let source = TestBlockBuilder::state().build_saved();
        let block = TestBlockBuilder::state().link(source.hash()).build();
        let ledger = Ledger::new_null_builder().block(&source).finish();
        let any = ledger.any();
        let validator =
            BlockValidatorFactory::new(&any, &ledger.constants, &block).create_validator();
        assert_eq!(validator.source_block_exists, true);
    }

    #[test]
    fn previous_block() {
        let previous = SavedBlock::new_test_instance();
        let block = TestBlockBuilder::state()
            .previous(previous.hash())
            .build_saved();
        let ledger = Ledger::new_null_builder().block(&previous).finish();
        let any = ledger.any();
        let validator =
            BlockValidatorFactory::new(&any, &ledger.constants, &block).create_validator();
        assert_eq!(validator.previous_block, Some(previous));
    }

    #[test]
    fn sends_skip_source_and_receivable_reads_but_preserve_pending_metadata() {
        let chain = SavedAccountChain::new_opened_chain();
        let source = TestBlockBuilder::state().build_saved();
        // A destination can have the same bytes as a pending send's hash. Keep the
        // existing source-epoch metadata even though this block is not a receive.
        let send = chain.new_send_block().link(source.hash()).build();
        let ledger = Ledger::new_null_builder()
            .block(chain.latest_block())
            .account_info(&chain.account(), &chain.account_info())
            .block(&source)
            .pending(
                &PendingKey::new(chain.account(), source.hash()),
                &PendingInfo::new(source.account(), Amount::raw(1), Epoch::Epoch1),
            )
            .finish();

        let instructions = validate_with_read_counts(&ledger, &send, 0, 0).unwrap();
        assert_eq!(instructions.set_sideband.source_epoch, Epoch::Epoch1);
        assert_eq!(instructions.set_account_info.epoch, Epoch::Epoch1);

        for block in [
            chain.new_legacy_send_block().build(),
            chain.new_state_block().build(),
            chain.new_legacy_change_block().build(),
            chain.new_epoch1_block().build(),
        ] {
            validate_with_read_counts(&ledger, &block, 0, 0).unwrap();
        }
    }

    #[test]
    fn receives_and_opens_check_the_source_without_scanning_receivables() {
        let source = TestBlockBuilder::state().build_saved();
        for opened in [false, true] {
            let chain = if opened {
                SavedAccountChain::new_opened_chain()
            } else {
                SavedAccountChain::new()
            };
            let blocks = if opened {
                [
                    chain.new_receive_block().link(source.hash()).build(),
                    chain
                        .new_legacy_receive_block()
                        .source(source.hash())
                        .build(),
                ]
            } else {
                [
                    chain
                        .new_open_block()
                        .balance(1)
                        .link(source.hash())
                        .build(),
                    chain.new_legacy_open_block().source(source.hash()).build(),
                ]
            };
            for source_exists in [false, true] {
                let mut builder = Ledger::new_null_builder().pending(
                    &PendingKey::new(chain.account(), source.hash()),
                    &PendingInfo::new(source.account(), Amount::raw(1), Epoch::Epoch0),
                );
                if opened {
                    builder = builder
                        .block(chain.latest_block())
                        .account_info(&chain.account(), &chain.account_info());
                }
                if source_exists {
                    builder = builder.block(&source);
                }
                let ledger = builder.finish();
                for block in &blocks {
                    let result = validate_with_read_counts(&ledger, block, 1, 0);
                    if source_exists {
                        assert!(result.is_ok(), "{result:?}");
                    } else {
                        assert_eq!(result, Err(BlockError::GapSource));
                    }
                }
            }
        }
    }

    #[test]
    fn epoch_opens_still_require_pending_and_preserve_error_order() {
        let chain = SavedAccountChain::new();
        for pending_exists in [false, true] {
            let mut builder = Ledger::new_null_builder();
            if pending_exists {
                builder = builder.pending(
                    &PendingKey::new(chain.account(), BlockHash::from(42)),
                    &PendingInfo::new(Account::from(1), Amount::raw(1), Epoch::Epoch0),
                );
            }
            let ledger = builder.finish();
            let block = chain.new_epoch1_open_block().build();
            let result = validate_with_read_counts(&ledger, &block, 0, 1);
            if pending_exists {
                assert!(result.is_ok(), "{result:?}");
            } else {
                assert_eq!(result, Err(BlockError::GapEpochOpenPending));
            }

            let malformed = chain.new_epoch1_open_block().representative(42).build();
            assert_eq!(
                validate_with_read_counts(&ledger, &malformed, 0, 1),
                Err(BlockError::RepresentativeMismatch)
            );
        }
    }

    #[test]
    fn malformed_state_blocks_keep_receive_classification_and_error_order() {
        let chain = SavedAccountChain::new_opened_chain();
        let ledger = Ledger::new_null_builder()
            .block(chain.latest_block())
            .account_info(&chain.account(), &chain.account_info())
            .finish();
        let bad_signature = chain.new_receive_block().sign_zero().build();
        assert_eq!(
            validate_with_read_counts(&ledger, &bad_signature, 1, 0),
            Err(BlockError::BadSignature)
        );

        // With no balance change, a nonzero ordinary link is still a receive.
        let zero_amount_receive = chain.new_state_block().link(123).build();
        assert_eq!(
            validate_with_read_counts(&ledger, &zero_amount_receive, 1, 0),
            Err(BlockError::GapSource)
        );

        // An epoch link can be an ordinary send. Missing predecessors retain
        // epoch precheck ordering, including signature failure before GapPrevious.
        for bad_signature in [false, true] {
            let mut builder = chain
                .new_send_block()
                .link(epoch_v1_link())
                .previous(BlockHash::from(999));
            if bad_signature {
                builder = builder.sign_zero();
            }
            assert_eq!(
                validate_with_read_counts(&ledger, &builder.build(), 0, 0),
                Err(if bad_signature {
                    BlockError::BadSignature
                } else {
                    BlockError::GapPrevious
                })
            );
        }
    }

    fn validate_with_read_counts(
        ledger: &Ledger,
        block: &Block,
        expected_source_reads: usize,
        expected_receivable_cursors: usize,
    ) -> Result<BlockInsertInstructions, BlockError> {
        let tx = ledger.store.begin_read();
        let tracked = TrackedReads {
            inner: &tx,
            gets: RefCell::new(Vec::new()),
            cursors: RefCell::new(Vec::new()),
        };
        let any = BorrowingAnySet {
            constants: &ledger.constants,
            store: &ledger.store,
            tx: &tracked,
        };
        let mut validator =
            BlockValidatorFactory::new(&any, &ledger.constants, block).create_validator();
        let source = block.source_or_link();
        let source_reads = tracked
            .gets
            .borrow()
            .iter()
            .filter(|(db, key)| *db == BLOCK_INDEX_DATABASE && key.as_slice() == source.as_bytes())
            .count();
        assert_eq!(source_reads, expected_source_reads);
        assert_eq!(
            tracked.cursors.borrow().as_slice(),
            vec![ledger.store.pending.database(); expected_receivable_cursors]
        );

        let result = validator.validate();
        // Compare the complete result (including sideband and account metadata)
        // with the same validator populated by the previous eager reads.
        validator.source_block_exists = !source.is_zero() && any.block_exists(&source);
        validator.any_pending_exists = any.receivable_exists(validator.account);
        assert_eq!(result, validator.validate());
        result
    }

    struct TrackedReads<'a> {
        inner: &'a dyn Transaction,
        gets: RefCell<Vec<(LmdbDatabase, Vec<u8>)>>,
        cursors: RefCell<Vec<LmdbDatabase>>,
    }

    impl Transaction for TrackedReads<'_> {
        fn is_refresh_needed(&self) -> bool {
            self.inner.is_refresh_needed()
        }

        fn is_refresh_needed_with(&self, max_duration: Duration) -> bool {
            self.inner.is_refresh_needed_with(max_duration)
        }

        fn get(&self, database: LmdbDatabase, key: &[u8]) -> rsnano_nullable_lmdb::Result<&[u8]> {
            self.gets.borrow_mut().push((database, key.to_vec()));
            self.inner.get(database, key)
        }

        fn open_ro_cursor(
            &self,
            database: LmdbDatabase,
        ) -> rsnano_nullable_lmdb::Result<RoCursor<'_>> {
            self.cursors.borrow_mut().push(database);
            self.inner.open_ro_cursor(database)
        }

        fn count(&self, database: LmdbDatabase) -> u64 {
            self.inner.count(database)
        }
    }
}
