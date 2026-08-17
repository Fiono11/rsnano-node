use std::sync::atomic::Ordering;

use rsnano_nullable_lmdb::WriteTransaction;
use rsnano_types::{
    Account, AccountInfo, Block, BlockPriority, BlockSideband, PendingInfo, PendingKey, SavedBlock,
};

use crate::Ledger;

#[derive(Debug, PartialEq)]
pub(crate) struct BlockInsertInstructions {
    pub account: Account,
    pub old_account_info: AccountInfo,
    pub set_account_info: AccountInfo,
    pub delete_pending: Option<PendingKey>,
    pub insert_pending: Option<(PendingKey, PendingInfo)>,
    pub set_sideband: BlockSideband,
    pub is_epoch_block: bool,
    pub priority: BlockPriority,
}

/// Inserts a new block into the ledger
pub(crate) struct BlockInserter<'a> {
    ledger: &'a Ledger,
    txn: &'a mut WriteTransaction,
    block: &'a Block,
    instructions: &'a BlockInsertInstructions,
}

impl<'a> BlockInserter<'a> {
    pub(crate) fn new(
        ledger: &'a Ledger,
        txn: &'a mut WriteTransaction,
        block: &'a Block,
        instructions: &'a BlockInsertInstructions,
    ) -> Self {
        Self {
            ledger,
            txn,
            block,
            instructions,
        }
    }

    pub(crate) fn insert(&mut self) -> Option<(SavedBlock, BlockPriority)> {
        if self.account_changed_since_validation() {
            return None;
        }

        let sideband = self.instructions.set_sideband.clone();
        let saved_block = SavedBlock::new(self.block.clone(), sideband);
        self.ledger.store.block.put(self.txn, &saved_block);
        if !saved_block.previous().is_zero() {
            self.ledger.store.successors.put(
                self.txn,
                &saved_block.previous(),
                &saved_block.hash(),
            );
        }
        self.update_account();
        self.delete_old_pending_info();
        self.insert_new_pending_info();
        #[cfg(not(feature = "rai_protocol"))]
        self.update_rep_weights();
        self.ledger
            .store
            .cache
            .block_count
            .fetch_add(1, Ordering::SeqCst);

        Some((saved_block, self.instructions.priority))
    }

    fn account_changed_since_validation(&mut self) -> bool {
        if let Some(pending) = &self.instructions.delete_pending {
            // It could be that the corresponding send block got rolled back, so we
            // have to check the pending store!
            if !self.ledger.store.pending.exists(self.txn, pending) {
                return true;
            }
        }
        self.get_current_account_info().head != self.instructions.old_account_info.head
    }

    fn get_current_account_info(&mut self) -> AccountInfo {
        self.ledger
            .store
            .account
            .get(self.txn, &self.instructions.account)
            .unwrap_or_default()
    }

    fn update_account(&mut self) {
        self.ledger.update_account(
            self.txn,
            &self.instructions.account,
            &self.instructions.old_account_info,
            &self.instructions.set_account_info,
        );
    }

    fn delete_old_pending_info(&mut self) {
        if let Some(key) = &self.instructions.delete_pending {
            self.ledger.store.pending.del(self.txn, key);
        }
    }

    fn insert_new_pending_info(&mut self) {
        if let Some((key, info)) = &self.instructions.insert_pending {
            self.ledger.store.pending.put(self.txn, key, info);
        }
    }

    #[cfg(not(feature = "rai_protocol"))]
    fn update_rep_weights(&mut self) {
        if !self.instructions.old_account_info.head.is_zero() {
            // Move existing rep weight and add in amount delta
            self.ledger.rep_weights_updater.sub_and_add(
                self.txn,
                self.instructions.old_account_info.representative,
                self.instructions.old_account_info.balance,
                self.instructions.set_account_info.representative,
                self.instructions.set_account_info.balance,
            );
        } else {
            // Add in amount delta only
            self.ledger.rep_weights_updater.add(
                self.txn,
                self.instructions.set_account_info.representative,
                self.instructions.set_account_info.balance,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "rai_protocol"))]
    use rsnano_types::PublicKey;
    use rsnano_types::{BlockHash, Epoch, TestBlockBuilder, UnixTimestamp};

    #[test]
    #[cfg(not(feature = "rai_protocol"))]
    fn insert_open_state_block() {
        let (mut block, instructions) = open_state_block_instructions();
        let ledger = Ledger::new_null();

        let result = insert(&ledger, &mut block, &instructions);

        let expected_block = SavedBlock::new(block.clone(), instructions.set_sideband.clone());
        assert_eq!(result.saved_blocks, vec![expected_block]);
        assert_eq!(
            result.saved_accounts,
            vec![(instructions.account, instructions.set_account_info.clone())]
        );
        assert_eq!(
            ledger
                .rep_weights
                .weight(&instructions.set_account_info.representative),
            instructions.set_account_info.balance
        );
        assert_eq!(ledger.store.cache.block_count.load(Ordering::Relaxed), 2);
        assert_eq!(result.deleted_pending, Vec::new());
    }

    #[test]
    #[cfg(feature = "rai_protocol")]
    fn rai_insert_does_not_maintain_live_rep_weights() {
        let (mut block, instructions) = open_state_block_instructions();
        let ledger = Ledger::new_null();

        insert(&ledger, &mut block, &instructions);

        assert_eq!(
            ledger
                .rep_weights
                .weight(&instructions.set_account_info.representative),
            rsnano_types::Amount::ZERO
        );
        let txn = ledger.store.begin_read();
        assert_eq!(
            ledger
                .store
                .rep_weight
                .get(&txn, &instructions.set_account_info.representative),
            None
        );
    }

    #[test]
    fn delete_old_pending() {
        let (mut block, mut instructions) = legacy_open_block_instructions();
        let pending_key = PendingKey::new_test_instance();
        instructions.delete_pending = Some(pending_key.clone());
        let ledger = Ledger::new_null();
        let mut txn = ledger.store.env.begin_write();
        ledger
            .store
            .pending
            .put(&mut txn, &pending_key, &PendingInfo::new_test_instance());
        txn.commit();

        let result = insert(&ledger, &mut block, &instructions);

        assert_eq!(result.deleted_pending, vec![pending_key]);
    }

    #[test]
    fn doesnt_insert_if_pending_missing() {
        let (mut block, mut instructions) = legacy_open_block_instructions();
        let pending_key = PendingKey::new_test_instance();
        instructions.delete_pending = Some(pending_key.clone());
        let ledger = Ledger::new_null();

        let mut txn = ledger.store.env.begin_write();
        let mut block_inserter = BlockInserter::new(&ledger, &mut txn, &mut block, &instructions);
        let result = block_inserter.insert();
        txn.commit();
        assert!(result.is_none());
    }

    #[test]
    fn insert_pending() {
        let (mut block, mut instructions) = legacy_open_block_instructions();
        let pending_key = PendingKey::new_test_instance();
        let pending_info = PendingInfo::new_test_instance();
        instructions.insert_pending = Some((pending_key.clone(), pending_info.clone()));
        let ledger = Ledger::new_null();

        let result = insert(&ledger, &mut block, &instructions);

        assert_eq!(result.saved_pending, vec![(pending_key, pending_info)]);
    }

    #[test]
    #[cfg(not(feature = "rai_protocol"))]
    fn update_representative() {
        let old_representative = PublicKey::from(1111);
        let new_representative = PublicKey::from(2222);
        let open = TestBlockBuilder::legacy_open()
            .representative(old_representative)
            .build_saved();

        let state = TestBlockBuilder::state()
            .previous(open.hash())
            .representative(new_representative)
            .balance(open.balance())
            .build();
        let (mut state, instructions) = state_block_instructions_for(&open, state);

        let ledger = Ledger::new_null_builder()
            .block(&open)
            .account_info(
                &open.account(),
                &AccountInfo {
                    head: open.hash(),
                    representative: old_representative,
                    open_block: open.hash(),
                    balance: open.balance(),
                    modified: UnixTimestamp::new(1),
                    block_count: 1,
                    epoch: Epoch::Epoch0,
                },
            )
            .rep_weights([(open.representative_field().unwrap(), open.balance())])
            .finish();

        insert(&ledger, &mut state, &instructions);

        assert_eq!(
            ledger.rep_weights.weight(&new_representative),
            instructions.set_account_info.balance
        );
    }

    #[test]
    fn no_successor_for_open_block() {
        let (mut block, instructions) = open_state_block_instructions();
        let ledger = Ledger::new_null();

        let result = insert(&ledger, &mut block, &instructions);

        assert_eq!(result.saved_successors, Vec::new());
    }

    #[test]
    fn insert_successor() {
        let open = TestBlockBuilder::legacy_open().build_saved();

        let state = TestBlockBuilder::state()
            .previous(open.hash())
            .balance(open.balance())
            .build();
        let (mut state, instructions) = state_block_instructions_for(&open, state);

        let ledger = Ledger::new_null_builder()
            .block(&open)
            .account_info(
                &open.account(),
                &AccountInfo {
                    head: open.hash(),
                    representative: open.representative_field().unwrap(),
                    open_block: open.hash(),
                    balance: open.balance(),
                    modified: UnixTimestamp::new(1),
                    block_count: 1,
                    epoch: Epoch::Epoch0,
                },
            )
            .rep_weights([(open.representative_field().unwrap(), open.balance())])
            .finish();

        let result = insert(&ledger, &mut state, &instructions);

        assert_eq!(result.saved_successors, vec![(open.hash(), state.hash())]);
    }

    fn insert(
        ledger: &Ledger,
        block: &mut Block,
        instructions: &BlockInsertInstructions,
    ) -> InsertResult {
        let mut txn = ledger.store.begin_write();
        let saved_blocks = ledger.store.block.track_puts();
        let saved_accounts = ledger.store.account.track_puts();
        let saved_pending = ledger.store.pending.track_puts();
        let saved_successors = ledger.store.successors.track_puts();
        let deleted_pending = ledger.store.pending.track_deletions();

        let mut block_inserter = BlockInserter::new(&ledger, &mut txn, block, &instructions);
        block_inserter.insert().unwrap();
        txn.commit();

        InsertResult {
            saved_blocks: saved_blocks.output(),
            saved_accounts: saved_accounts.output(),
            saved_pending: saved_pending.output(),
            saved_successors: saved_successors.output(),
            deleted_pending: deleted_pending.output(),
        }
    }

    struct InsertResult {
        #[cfg_attr(feature = "rai_protocol", allow(dead_code))]
        saved_blocks: Vec<SavedBlock>,
        #[cfg_attr(feature = "rai_protocol", allow(dead_code))]
        saved_accounts: Vec<(Account, AccountInfo)>,
        saved_pending: Vec<(PendingKey, PendingInfo)>,
        saved_successors: Vec<(BlockHash, BlockHash)>,
        deleted_pending: Vec<PendingKey>,
    }

    fn legacy_open_block_instructions() -> (Block, BlockInsertInstructions) {
        let block = TestBlockBuilder::legacy_open().build();
        let sideband = BlockSideband::new_test_instance();
        let account_info = AccountInfo {
            head: block.hash(),
            open_block: block.hash(),
            ..AccountInfo::new_test_instance()
        };
        let instructions = BlockInsertInstructions {
            account: block.account_field().unwrap(),
            old_account_info: AccountInfo::default(),
            set_account_info: account_info,
            delete_pending: None,
            insert_pending: None,
            set_sideband: sideband,
            is_epoch_block: false,
            priority: BlockPriority::new_test_instance(),
        };

        (block, instructions)
    }

    fn open_state_block_instructions() -> (Block, BlockInsertInstructions) {
        let block = TestBlockBuilder::state().previous(BlockHash::ZERO).build();
        let sideband = BlockSideband::new_test_instance();
        let account_info = AccountInfo {
            head: block.hash(),
            open_block: block.hash(),
            ..AccountInfo::new_test_instance()
        };
        let instructions = BlockInsertInstructions {
            account: Account::from(1),
            old_account_info: AccountInfo::default(),
            set_account_info: account_info,
            delete_pending: None,
            insert_pending: None,
            set_sideband: sideband,
            is_epoch_block: false,
            priority: BlockPriority::new_test_instance(),
        };

        (block, instructions)
    }

    fn state_block_instructions_for(
        previous: &SavedBlock,
        block: Block,
    ) -> (Block, BlockInsertInstructions) {
        let sideband = BlockSideband {
            balance: block.balance_field().unwrap(),
            account: block.account_field().unwrap(),
            ..BlockSideband::new_test_instance()
        };
        let old_account_info = AccountInfo {
            head: previous.hash(),
            balance: previous.balance(),
            representative: previous.representative_field().unwrap(),
            ..AccountInfo::new_test_instance()
        };
        let new_account_info = AccountInfo {
            head: block.hash(),
            open_block: previous.hash(),
            balance: block.balance_field().unwrap(),
            representative: block.representative_field().unwrap(),
            ..AccountInfo::new_test_instance()
        };
        let instructions = BlockInsertInstructions {
            account: previous.account(),
            old_account_info,
            set_account_info: new_account_info,
            delete_pending: None,
            insert_pending: None,
            set_sideband: sideband,
            is_epoch_block: false,
            priority: BlockPriority::new_test_instance(),
        };

        (block, instructions)
    }
}
