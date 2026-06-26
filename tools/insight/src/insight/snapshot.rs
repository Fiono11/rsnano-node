use rsnano_ledger::BlockSource;
use rsnano_node::{
    Node,
    bootstrap::bootstrapper::PeerScoreSnapshot,
    cementation::ConfirmingSetInfo,
    consensus::{ActiveElectionsInfo, AecSnapshot, RepTier},
};
use rsnano_utils::fair_queue::FairQueueInfo;

#[derive(Default)]
pub(crate) struct InsightSnapshot {
    pub aec_info: ActiveElectionsInfo,
    pub max_optimistic: usize,
    pub max_hinted: usize,
    pub confirming_set: ConfirmingSetInfo,
    pub block_processor_info: FairQueueInfo<BlockSource>,
    pub vote_processor_info: FairQueueInfo<RepTier>,
    pub elections: AecSnapshot,
    pub peer_scores: Vec<PeerScoreSnapshot>,
    pub ledger_stats: LedgerStats,
}

#[derive(Default)]
pub(crate) struct LedgerStats {
    pub total_blocks: u64,
    pub confirmed_blocks: u64,
    pub account_count: u64,
    pub bps: i64,
    pub cps: i64,
}

pub(crate) fn take_snapshot(node: &Node) -> InsightSnapshot {
    InsightSnapshot {
        aec_info: node.aec.info(),
        max_optimistic: node.election_schedulers.optimistic.max_elections(),
        max_hinted: node.election_schedulers.hinted.max_elections,
        confirming_set: node.confirming_set.info(),
        block_processor_info: node.block_processor_queue.info(),
        vote_processor_info: node.vote_processor_queue.info(),
        elections: node.aec.snapshot(),
        peer_scores: node.bootstrapper.peer_score_snapshot(),
        ledger_stats: take_ledger_stats(node),
    }
}

fn take_ledger_stats(node: &Node) -> LedgerStats {
    LedgerStats {
        total_blocks: node.ledger.block_count(),
        confirmed_blocks: node.ledger.confirmed_count(),
        account_count: node.ledger.account_count(),
        bps: node.block_rates.bps(),
        cps: node.block_rates.cps(),
    }
}
