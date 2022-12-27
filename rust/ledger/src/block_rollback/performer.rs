use rsnano_core::{AccountInfo, BlockEnum, BlockHash};
use rsnano_store_traits::WriteTransaction;

use crate::Ledger;

use super::{
    applier::RollbackInstructionsApplier, planner::RollbackStep,
    planner_factory::RollbackPlannerFactory,
};

pub(crate) struct BlockRollbackPerformer<'a> {
    ledger: &'a Ledger,
    pub txn: &'a mut dyn WriteTransaction,
    pub rolled_back: Vec<BlockEnum>,
}

impl<'a> BlockRollbackPerformer<'a> {
    pub(crate) fn new(ledger: &'a Ledger, txn: &'a mut dyn WriteTransaction) -> Self {
        Self {
            ledger,
            txn,
            rolled_back: Vec::new(),
        }
    }

    pub(crate) fn roll_back(mut self, block_hash: &BlockHash) -> anyhow::Result<Vec<BlockEnum>> {
        self.recurse_roll_back(block_hash)?;
        Ok(self.rolled_back)
    }

    fn recurse_roll_back(&mut self, block_hash: &BlockHash) -> anyhow::Result<()> {
        let block = self.load_block(block_hash)?;
        while self.block_exists(block_hash) {
            let head_block = self.load_account_head(&block)?;
            self.roll_back_head_block(head_block)?;
        }
        Ok(())
    }

    fn roll_back_head_block(&mut self, head_block: BlockEnum) -> Result<(), anyhow::Error> {
        let planner = RollbackPlannerFactory::new(self.ledger, self.txn.txn(), &head_block)
            .create_planner()?;
        let step = planner.roll_back_head_block()?;
        self.execute(step, head_block)?;
        Ok(())
    }

    fn execute(&mut self, step: RollbackStep, head_block: BlockEnum) -> Result<(), anyhow::Error> {
        Ok(match step {
            RollbackStep::RollBackBlock(instructions) => {
                RollbackInstructionsApplier::new(self.ledger, self.txn, &instructions).apply();
                self.rolled_back.push(head_block);
            }
            RollbackStep::RequestDependencyRollback(hash) => self.recurse_roll_back(&hash)?,
        })
    }

    fn block_exists(&self, block_hash: &BlockHash) -> bool {
        self.ledger.store.block().exists(self.txn.txn(), block_hash)
    }

    fn load_account_head(&self, block: &BlockEnum) -> anyhow::Result<BlockEnum> {
        let account_info = self.get_account_info(block);
        self.load_block(&account_info.head)
    }

    fn get_account_info(&self, block: &BlockEnum) -> AccountInfo {
        self.ledger
            .store
            .account()
            .get(self.txn.txn(), &block.account_calculated())
            .unwrap()
    }

    fn load_block(&self, block_hash: &BlockHash) -> anyhow::Result<BlockEnum> {
        self.ledger
            .store
            .block()
            .get(self.txn.txn(), block_hash)
            .ok_or_else(|| anyhow!("block not found"))
    }
}
