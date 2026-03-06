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

    pub(super) fn walk_backwards<T>(&mut self, start: BlockHash, end: BlockHash, mut handle: T)
    where
        T: FnMut(&SavedBlock, BlockPriority) -> bool,
    {
        let mut block = self.any.get_block(&start);

        while let Some(blk) = block {
            if blk.hash() == end {
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
