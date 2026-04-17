use rsnano_utils::stats::{StatsCollection, StatsSource};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub(crate) struct BootstrapRequesterStats {
    pub wait_block_processor: AtomicU64,
    pub wait_next_download: AtomicU64,
    pub no_channel: AtomicU64,
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
        result.insert(
            STAT_NAME,
            "wait_next_download",
            self.wait_next_download.load(Relaxed),
        );
        result.insert(STAT_NAME, "no_channel", self.no_channel.load(Relaxed));

        result.insert(
            STAT_NAME,
            "queries_overfill",
            self.queries_overfill.load(Relaxed),
        );

        result.insert(STAT_NAME, "rate_limit", self.rate_limit.load(Relaxed));
        result.insert(STAT_NAME, "sleep", self.sleep.load(Relaxed));
    }
}
