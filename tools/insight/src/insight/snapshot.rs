use rsnano_node::{Node, bootstrap::bootstrapper::PeerScoreSnapshot, consensus::AecSnapshot};

#[derive(Default)]
pub(crate) struct InsightSnapshot {
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
