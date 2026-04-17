use super::{PriorityDownResult, PriorityUpResult};
use rsnano_utils::stats::{StatsCollection, StatsSource};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub(crate) struct BootstrapQueueStats {
    pub inserted: AtomicU64,
    pub upgraded: AtomicU64,
    pub invalid_account: AtomicU64,
    pub removed: AtomicU64,
    pub remove_failed: AtomicU64,
    pub deprioritized: AtomicU64,
    pub not_found: AtomicU64,
    pub blocked: AtomicU64,
    pub block_failed: AtomicU64,
    pub unblocked: AtomicU64,
    pub decayed_blocked: AtomicU64,
    pub dependency_update: AtomicU64,
    pub dependency_update_failed: AtomicU64,
}

impl BootstrapQueueStats {
    pub fn add_prio_set_result(&mut self, result: &PriorityUpResult) {
        match result {
            PriorityUpResult::Inserted => self.inserted.fetch_add(1, Relaxed),
            PriorityUpResult::Upgraded => self.upgraded.fetch_add(1, Relaxed),
            PriorityUpResult::InvalidAccount => self.invalid_account.fetch_add(1, Relaxed),
            PriorityUpResult::Unchanged => 0,
        };
    }

    pub fn add_prio_down_result(&mut self, result: &PriorityDownResult) {
        match result {
            PriorityDownResult::Deprioritized => self.deprioritized.fetch_add(1, Relaxed),
            PriorityDownResult::Removed => self.removed.fetch_add(1, Relaxed),
            PriorityDownResult::AccountNotFound => self.not_found.fetch_add(1, Relaxed),
        };
    }
}

impl StatsSource for BootstrapQueueStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(KEY, "inserted", self.inserted.load(Relaxed));
        result.insert(KEY, "upgraded", self.upgraded.load(Relaxed));
        result.insert(KEY, "invalid_account", self.invalid_account.load(Relaxed));
        result.insert(KEY, "removed", self.removed.load(Relaxed));
        result.insert(KEY, "remove_failed", self.remove_failed.load(Relaxed));
        result.insert(KEY, "deprioritized", self.deprioritized.load(Relaxed));
        result.insert(KEY, "not_found", self.not_found.load(Relaxed));
        result.insert(KEY, "blocked", self.blocked.load(Relaxed));
        result.insert(KEY, "block_failed", self.block_failed.load(Relaxed));
        result.insert(KEY, "unblocked", self.unblocked.load(Relaxed));
        result.insert(KEY, "decayed_blocked", self.decayed_blocked.load(Relaxed));
        result.insert(
            KEY,
            "dependency_update",
            self.dependency_update.load(Relaxed),
        );
        result.insert(
            KEY,
            "dependency_update_failed",
            self.dependency_update_failed.load(Relaxed),
        );
    }
}

static KEY: &str = "bootstrap_queue";
