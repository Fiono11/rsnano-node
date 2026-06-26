use rsnano_node::{Node, consensus::ActiveElectionsInfo};

#[derive(Default)]
pub(crate) struct InsightSnapshot {
    pub aec_info: ActiveElectionsInfo,
    pub max_optimistic: usize,
}

pub(crate) fn take_snapshot(node: &Node) -> InsightSnapshot {
    InsightSnapshot {
        aec_info: node.aec.info(),
        max_optimistic: node.election_schedulers.optimistic.max_elections(),
    }
}
