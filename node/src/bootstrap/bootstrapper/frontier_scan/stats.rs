use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
};

use rsnano_types::Account;
use rsnano_utils::stats::{StatsCollection, StatsSource};

use crate::bootstrap::bootstrapper::logic::frontiers_processor::OutdatedAccounts;

#[derive(Default)]
pub(crate) struct FrontierScanStats {
    pub processed_frontiers: AtomicU64,
    pub verified: AtomicU64,
    pub nothing_new: AtomicU64,
    pub invalid: AtomicU64,
    pub outdated_accounts_found: AtomicU64,
    last: Mutex<VecDeque<Account>>,
}

impl FrontierScanStats {
    pub fn add(&self, outdated: &OutdatedAccounts) {
        let mut last = self.last.lock().unwrap();
        for account in &outdated.accounts {
            last.push_back(*account);
            if last.len() > 20 {
                last.pop_front();
            }
        }
    }

    pub fn last_outdated_found(&self) -> Vec<Account> {
        self.last.lock().unwrap().iter().cloned().collect()
    }
}

impl StatsSource for FrontierScanStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        const KEY: &str = "bootstrap_frontiers";
        result.insert(KEY, "ok", self.verified.load(Relaxed));
        result.insert(KEY, "nothing_new", self.nothing_new.load(Relaxed));
        result.insert(KEY, "invalid", self.invalid.load(Relaxed));
        result.insert(KEY, "frontiers", self.processed_frontiers.load(Relaxed));
        result.insert(KEY, "outdated", self.outdated_accounts_found.load(Relaxed));
    }
}
