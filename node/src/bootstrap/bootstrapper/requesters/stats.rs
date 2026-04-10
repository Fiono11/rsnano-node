use rsnano_utils::stats::{StatsCollection, StatsSource};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub(crate) struct BootstrapRequesterStats {
    pub wait_block_processor: AtomicU64,
    pub wait_priority: AtomicU64,
    pub next: AtomicU64,
    pub no_candidate: AtomicU64,
    pub queries_overfill: AtomicU64,
    pub rate_limit: AtomicU64,
    pub sleep: AtomicU64,
}

impl StatsSource for BootstrapRequesterStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const STAT_NAME: &str = "boot_requester";

        result.insert(
            STAT_NAME,
            "wait_block_processor",
            self.wait_block_processor.load(Relaxed),
        );
        result.insert(STAT_NAME, "wait_priority", self.wait_priority.load(Relaxed));
        result.insert(STAT_NAME, "next_priority", self.next.load(Relaxed));
        result.insert(STAT_NAME, "no_candidate", self.no_candidate.load(Relaxed));

        result.insert(
            STAT_NAME,
            "queries_overfill",
            self.queries_overfill.load(Relaxed),
        );

        result.insert(STAT_NAME, "rate_limit", self.queries_overfill.load(Relaxed));
        result.insert(STAT_NAME, "sleep", self.sleep.load(Relaxed));
    }
}
