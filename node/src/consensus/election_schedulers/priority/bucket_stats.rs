use rsnano_utils::stats::{StatsCollection, StatsSource};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct BucketStats {
    pub cancelled: AtomicU64,
}

impl StatsSource for BucketStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(STATS_KEY, "cancel_lowest", self.cancelled.load(Relaxed));
    }
}

const STATS_KEY: &str = "election_bucket";
