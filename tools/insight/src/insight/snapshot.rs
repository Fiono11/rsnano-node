use rsnano_node::{
    Node,
    cementation::ConfirmingSetInfo,
    consensus::{ActiveElectionsInfo, RepTier},
};
use rsnano_ledger::BlockSource;
use rsnano_utils::fair_queue::FairQueueInfo;

#[derive(Default)]
pub(crate) struct InsightSnapshot {
    pub aec_info: ActiveElectionsInfo,
    pub max_optimistic: usize,
    pub max_hinted: usize,
    pub confirming_set: ConfirmingSetInfo,
    pub block_processor_info: FairQueueInfo<BlockSource>,
    pub vote_processor_info: FairQueueInfo<RepTier>,
}

pub(crate) fn take_snapshot(node: &Node) -> InsightSnapshot {
    InsightSnapshot {
        aec_info: node.aec.info(),
        max_optimistic: node.election_schedulers.optimistic.max_elections(),
        max_hinted: node.election_schedulers.hinted.max_elections,
        confirming_set: node.confirming_set.info(),
        block_processor_info: node.block_processor_queue.info(),
        vote_processor_info: node.vote_processor_queue.info(),
    }
}
