use rsnano_ledger::{AnySet, Ledger, OwningAnySet};
use rsnano_types::{BlockHash, BlockPriority, SavedBlock};

/// Walks the specified block range of an account from newest to oldest block
pub(super) struct AccountWalker<'a> {
    ledger: &'a Ledger,
    any: OwningAnySet<'a>,
}

impl<'a> AccountWalker<'a> {
    pub(super) fn new(ledger: &'a Ledger) -> Self {
        Self {
            ledger,
            any: ledger.any(),
        }
    }

    pub(super) fn walk_backwards<T>(
        &mut self,
        inclusive_start: BlockHash,
        exclusive_end: BlockHash,
        mut handle: T,
    ) where
        T: FnMut(&SavedBlock, BlockPriority) -> bool,
    {
        let mut block = self.any.get_block(&inclusive_start);

        while let Some(blk) = block {
            if blk.hash() == exclusive_end {
                break;
            }

            let priority = self.any.block_priority(&blk);
            let should_continue = handle(&blk, priority);

            if !should_continue {
                break;
            }

            if self.any.should_refresh() {
                self.any = self.ledger.any();
            }

            block = self.any.get_block(&blk.previous());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_ledger::test_helpers::UnsavedBlockLatticeBuilder;
    use rsnano_types::PrivateKey;

    #[test]
    fn walks_nothing_when_start_block_not_found() {
        let ledger = Ledger::new_null();
        let mut walker = AccountWalker::new(&ledger);
        let mut callback_called = false;
        walker.walk_backwards(2.into(), 1.into(), |_, _| {
            callback_called = true;
            false
        });
        assert!(!callback_called);
    }

    #[test]
    fn walks_nothing_when_start_equals_end() {
        let ledger = Ledger::new_null();
        let mut builder = UnsavedBlockLatticeBuilder::with_stub_work();
        let account = PrivateKey::from(123);
        let genesis_send = builder.genesis().send(&account, 1000);
        let open = builder.account(&account).receive(&genesis_send);

        ledger.process_one(&genesis_send).unwrap();
        ledger.process_one(&open).unwrap();

        let mut walker = AccountWalker::new(&ledger);
        let mut callback_called = false;
        walker.walk_backwards(open.hash(), open.hash(), |_, _| {
            callback_called = true;
            true
        });
        assert!(!callback_called);
    }

    #[test]
    fn walks_entire_chain_when_end_is_zero() {
        let ledger = Ledger::new_null();
        let mut builder = UnsavedBlockLatticeBuilder::with_stub_work();
        let account = PrivateKey::from(123);
        let genesis_send = builder.genesis().send(&account, 1000);
        let open = builder.account(&account).receive(&genesis_send);
        let send1 = builder.account(&account).send(42, 1);
        let send2 = builder.account(&account).send(42, 1);

        ledger.process_one(&genesis_send).unwrap();
        ledger.process_one(&open).unwrap();
        ledger.process_one(&send1).unwrap();
        ledger.process_one(&send2).unwrap();

        let mut walker = AccountWalker::new(&ledger);
        let mut walked = Vec::new();
        walker.walk_backwards(send2.hash(), BlockHash::ZERO, |block, _| {
            walked.push(block.hash());
            true
        });
        assert_eq!(walked, vec![send2.hash(), send1.hash(), open.hash()]);
    }

    #[test]
    fn end_block_is_exclusive() {
        let ledger = Ledger::new_null();
        let mut builder = UnsavedBlockLatticeBuilder::with_stub_work();
        let account = PrivateKey::from(123);
        let genesis_send = builder.genesis().send(&account, 1000);
        let open = builder.account(&account).receive(&genesis_send);
        let send1 = builder.account(&account).send(42, 1);
        let send2 = builder.account(&account).send(42, 1);
        let send3 = builder.account(&account).send(42, 1);

        ledger.process_one(&genesis_send).unwrap();
        ledger.process_one(&open).unwrap();
        ledger.process_one(&send1).unwrap();
        ledger.process_one(&send2).unwrap();
        ledger.process_one(&send3).unwrap();

        let mut walker = AccountWalker::new(&ledger);
        let mut walked = Vec::new();
        walker.walk_backwards(send2.hash(), open.hash(), |block, _| {
            walked.push(block.hash());
            true
        });
        assert_eq!(walked, vec![send2.hash(), send1.hash()]);
    }

    #[test]
    fn stops_early_when_callback_returns_false() {
        let ledger = Ledger::new_null();
        let mut builder = UnsavedBlockLatticeBuilder::with_stub_work();
        let account = PrivateKey::from(123);
        let genesis_send = builder.genesis().send(&account, 1000);
        let open = builder.account(&account).receive(&genesis_send);
        let send1 = builder.account(&account).send(42, 1);
        let send2 = builder.account(&account).send(42, 1);

        ledger.process_one(&genesis_send).unwrap();
        ledger.process_one(&open).unwrap();
        ledger.process_one(&send1).unwrap();
        ledger.process_one(&send2).unwrap();

        let mut walker = AccountWalker::new(&ledger);
        let mut walked = Vec::new();
        walker.walk_backwards(send2.hash(), BlockHash::ZERO, |block, _| {
            walked.push(block.hash());
            false
        });
        assert_eq!(walked, vec![send2.hash()]);
    }
}
