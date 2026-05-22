use rsnano_node::{
    bootstrap::bootstrapper::{Bootstrapper, FrontierHeadInfo, FrontierScanSnapshot},
    utils::RateCalculator,
};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::Account;

#[derive(Default)]
pub(crate) struct FrontierScanInfo {
    frontiers_rate: RateCalculator,
    outdated_rate: RateCalculator,
    pub frontier_heads: Vec<FrontierHeadInfo>,
    pub frontiers_total: u64,
    pub outdated_total: u64,
    pub outdated_accounts: Vec<Account>,
}

impl FrontierScanInfo {
    pub(crate) fn update(&mut self, bootstrapper: &Bootstrapper, now: Timestamp) {
        let snapshot = bootstrapper.frontier_scan_snapshot();
        self.update_counters(&snapshot, now);
        self.frontier_heads = snapshot.heads;
        self.outdated_accounts = snapshot.last_outdated_accounts;
    }

    fn update_counters(&mut self, snapshot: &FrontierScanSnapshot, now: Timestamp) {
        self.frontiers_rate
            .sample(snapshot.processed_frontiers, now);
        self.outdated_rate
            .sample(snapshot.outdated_accounts_found, now);
        self.frontiers_total = snapshot.processed_frontiers;
        self.outdated_total = snapshot.outdated_accounts_found;
    }

    pub(crate) fn frontiers_rate(&self) -> i64 {
        self.frontiers_rate.rate()
    }

    pub(crate) fn outdated_rate(&self) -> i64 {
        self.outdated_rate.rate()
    }
}
