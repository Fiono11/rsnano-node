use std::sync::Arc;

use rsnano_core::{Block, BlockSubType, BlockType};
use rsnano_ledger::LedgerObserver;

use super::{DetailType, Direction, Stat, StatType};

pub struct LedgerStats {
    stats: Arc<Stat>,
}

impl LedgerStats {
    pub fn new(stats: Arc<Stat>) -> Self {
        Self { stats }
    }
}

impl LedgerObserver for LedgerStats {
    fn blocks_cemented(&self, cemented_count: u64) {
        let _ = self.stats.add(
            StatType::ConfirmationHeight,
            DetailType::BlocksConfirmed,
            Direction::In,
            cemented_count,
            false,
        );

        let _ = self.stats.add(
            StatType::ConfirmationHeight,
            DetailType::BlocksConfirmedBounded,
            Direction::In,
            cemented_count,
            false,
        );
    }

    fn block_rolled_back(&self, block_type: BlockSubType) {
        let _ = self
            .stats
            .inc(StatType::Rollback, block_type.into(), Direction::In);
    }

    fn block_added(&self, block: &dyn Block, is_epoch: bool) {
        let _ = self.stats.inc(
            StatType::Ledger,
            block_detail_type(block, is_epoch),
            Direction::In,
        );
    }
}

fn block_detail_type(block: &dyn Block, is_epoch: bool) -> DetailType {
    match block.block_type() {
        BlockType::LegacySend => DetailType::Send,
        BlockType::LegacyReceive => DetailType::Receive,
        BlockType::LegacyOpen => DetailType::Open,
        BlockType::LegacyChange => DetailType::Change,
        BlockType::State => {
            if is_epoch {
                DetailType::EpochBlock
            } else {
                DetailType::StateBlock
            }
        }
        BlockType::Invalid | BlockType::NotABlock => unreachable!(),
    }
}

impl From<BlockSubType> for DetailType {
    fn from(block_type: BlockSubType) -> Self {
        match block_type {
            BlockSubType::Send => DetailType::Send,
            BlockSubType::Receive => DetailType::Receive,
            BlockSubType::Open => DetailType::Open,
            BlockSubType::Change => DetailType::Change,
            BlockSubType::Epoch => DetailType::EpochBlock,
        }
    }
}
