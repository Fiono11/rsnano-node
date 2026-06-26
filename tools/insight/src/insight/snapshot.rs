use rsnano_node::{Node, cementation::ConfirmingSetInfo, consensus::ActiveElectionsInfo};

#[derive(Default)]
pub(crate) struct InsightSnapshot {
    pub aec_info: ActiveElectionsInfo,
    pub max_optimistic: usize,
    pub max_hinted: usize,
    pub confirming_set: ConfirmingSetInfo,
}

pub(crate) fn take_snapshot(node: &Node) -> InsightSnapshot {
    InsightSnapshot {
        aec_info: node.aec.info(),
        max_optimistic: node.election_schedulers.optimistic.max_elections(),
        max_hinted: node.election_schedulers.hinted.max_elections,
        confirming_set: node.confirming_set.info(),
    }
}
