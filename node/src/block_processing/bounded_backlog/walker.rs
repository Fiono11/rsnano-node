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
        let walked = test_walk(&ledger, 2.into(), 1.into(), BlockHash::ZERO);
        assert_eq!(walked, vec![]);
    }

    #[test]
    fn walks_nothing_when_start_equals_end() {
        let ledger = Ledger::new_null();
        let chain = build_account_chain(&ledger, 1);

        let walked = test_walk(&ledger, chain[0], chain[0], BlockHash::ZERO);

        assert_eq!(walked, vec![]);
    }

    #[test]
    fn walks_entire_chain_when_end_is_zero() {
        let ledger = Ledger::new_null();
        let chain = build_account_chain(&ledger, 3);

        let walked = test_walk(&ledger, chain[2], BlockHash::ZERO, BlockHash::ZERO);

        assert_eq!(walked, vec![chain[2], chain[1], chain[0]]);
    }

    #[test]
    fn end_block_is_exclusive() {
        let ledger = Ledger::new_null();
        let chain = build_account_chain(&ledger, 4);

        let walked = test_walk(&ledger, chain[2], chain[0], BlockHash::ZERO);

        assert_eq!(walked, vec![chain[2], chain[1]]);
    }

    #[test]
    fn stops_early_when_callback_returns_false() {
        let ledger = Ledger::new_null();
        let chain = build_account_chain(&ledger, 3);

        let walked = test_walk(&ledger, chain[2], BlockHash::ZERO, chain[2]);

        assert_eq!(walked, vec![chain[2]]);
    }

    /*
     * Helpers
     */

    fn build_account_chain(ledger: &Ledger, num_blocks: usize) -> Vec<BlockHash> {
        assert_ne!(num_blocks, 0);

        let mut builder = UnsavedBlockLatticeBuilder::with_stub_work();
        let account = PrivateKey::from(123);
        let mut chain = Vec::new();

        let genesis_send = builder.genesis().send(&account, 1000);
        ledger.process_one(&genesis_send).unwrap();

        let open = builder.account(&account).receive(&genesis_send);
        ledger.process_one(&open).unwrap();
        chain.push(open.hash());

        for _ in 1..num_blocks {
            let block = builder.account(&account).send(42, 1);
            ledger.process_one(&block).unwrap();
            chain.push(block.hash());
        }

        chain
    }

    fn test_walk(
        ledger: &Ledger,
        start: BlockHash,
        end: BlockHash,
        early_exit: BlockHash,
    ) -> Vec<BlockHash> {
        let mut walker = AccountWalker::new(&ledger);
        let mut walked = Vec::new();
        walker.walk_backwards(start, end, |block, _| {
            walked.push(block.hash());
            block.hash() != early_exit
        });
        walked
    }
}
