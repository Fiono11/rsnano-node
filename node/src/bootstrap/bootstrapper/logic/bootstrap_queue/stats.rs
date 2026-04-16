use super::{PriorityDownResult, PrioritySetResult};
use rsnano_utils::stats::{StatsCollection, StatsSource};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub(crate) struct BootstrapQueueStats {
    pub inserted: AtomicU64,
    pub updated: AtomicU64,
    pub invalid_account: AtomicU64,
    pub removed: AtomicU64,
    pub unchanged: AtomicU64,
    pub deprioritized: AtomicU64,
    pub not_found: AtomicU64,
}

impl BootstrapQueueStats {
    pub fn add_prio_set_result(&mut self, result: &PrioritySetResult) {
        match result {
            PrioritySetResult::Inserted => self.inserted.fetch_add(1, Relaxed),
            PrioritySetResult::Updated => self.updated.fetch_add(1, Relaxed),
            PrioritySetResult::InvalidAccount => self.invalid_account.fetch_add(1, Relaxed),
            PrioritySetResult::Removed => self.removed.fetch_add(1, Relaxed),
            PrioritySetResult::Unchanged => self.unchanged.fetch_add(1, Relaxed),
        };
    }

    pub fn add_prio_down_result(&mut self, result: &PriorityDownResult) {
        match result {
            PriorityDownResult::Deprioritized => self.deprioritized.fetch_add(1, Relaxed),
            PriorityDownResult::Removed => self.removed.fetch_add(1, Relaxed),
            PriorityDownResult::AccountNotFound => self.not_found.fetch_add(1, Relaxed),
            PriorityDownResult::Unchanged => self.unchanged.fetch_add(1, Relaxed),
        };
    }
}

impl StatsSource for BootstrapQueueStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(KEY, "inserted", self.inserted.load(Relaxed));
        result.insert(KEY, "updated", self.updated.load(Relaxed));
        result.insert(KEY, "invalid_account", self.invalid_account.load(Relaxed));
        result.insert(KEY, "removed", self.removed.load(Relaxed));
        result.insert(KEY, "unchanged", self.unchanged.load(Relaxed));
        result.insert(KEY, "deprioritized", self.deprioritized.load(Relaxed));
        result.insert(KEY, "not_found", self.not_found.load(Relaxed));
    }
}

static KEY: &str = "bootstrap_queue";
