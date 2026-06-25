use rsnano_utils::stats::{StatsCollection, StatsSource};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub(crate) struct VoteCacheStats {
    pub inserted: AtomicU64,
    pub top: AtomicU64,
    pub processor_overfill: AtomicU64,
    pub triggered: AtomicU64,
    pub processed: AtomicU64,
}

impl StatsSource for VoteCacheStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert("vote_cache", "inserted", self.inserted.load(Relaxed));
        result.insert("vote_cache", "top", self.top.load(Relaxed));
        result.insert(
            "vote_cache",
            "processor_overfill",
            self.processor_overfill.load(Relaxed),
        );
        result.insert("vote_cache", "triggered", self.triggered.load(Relaxed));
        result.insert("vote_cache", "processed", self.processed.load(Relaxed));
    }
}
