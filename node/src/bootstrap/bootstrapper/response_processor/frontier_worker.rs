use std::sync::Mutex;

use rsnano_ledger::OwningAnySet;
use rsnano_types::Frontier;
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::frontier_checker::FrontierChecker;
use crate::bootstrap::bootstrapper::logic::{
    BootstrapLogic, BootstrapQueue, frontiers_processor::OutdatedAccounts,
};

/// Handles received frontiers
pub(crate) struct FrontierWorker<'a> {
    stats: &'a Stats,
    state: &'a Mutex<BootstrapLogic>,
    checker: FrontierChecker<'a>,
    bootstrap_queue: &'a BootstrapQueue,
}

impl<'a> FrontierWorker<'a> {
    pub(crate) fn new(
        any: &'a OwningAnySet<'a>,
        stats: &'a Stats,
        state: &'a Mutex<BootstrapLogic>,
        bootstrap_queue: &'a BootstrapQueue,
    ) -> Self {
        Self {
            stats,
            state,
            checker: FrontierChecker::new(any),
            bootstrap_queue,
        }
    }

    pub fn process(&mut self, frontiers: Vec<Frontier>) {
        let outdated = self.checker.get_outdated_accounts(&frontiers);
        self.update_stats(&frontiers, &outdated);
        self.state
            .lock()
            .unwrap()
            .frontiers_processor
            .frontiers_processed(&outdated, self.bootstrap_queue);
    }

    fn update_stats(&self, frontiers: &[Frontier], outdated: &OutdatedAccounts) {
        self.stats.add(
            StatType::BootstrapFrontiers,
            DetailType::Processed,
            frontiers.len() as u64,
        );
        self.stats.add(
            StatType::BootstrapFrontiers,
            DetailType::Prioritized,
            outdated.accounts.len() as u64,
        );
        self.stats.add(
            StatType::BootstrapFrontiers,
            DetailType::Outdated,
            outdated.outdated as u64,
        );
        self.stats.add(
            StatType::BootstrapFrontiers,
            DetailType::Pending,
            outdated.pending as u64,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap::bootstrapper::logic::{BootstrapQueue, Priority};
    use rsnano_ledger::Ledger;
    use rsnano_types::{Account, AccountInfo, BlockHash};
    use std::sync::Arc;

    #[test]
    fn empty() {
        let ledger = Ledger::new_null();
        let any = ledger.any();
        let stats = Stats::default();
        let bootstrap_queue = Arc::new(BootstrapQueue::new_null());
        let state = Mutex::new(BootstrapLogic::new(
            Default::default(),
            bootstrap_queue.clone(),
        ));
        let mut worker = FrontierWorker::new(&any, &stats, &state, &bootstrap_queue);

        worker.process(Vec::new());

        assert_eq!(bootstrap_queue.info().download_queue, 0);
    }

    #[test]
    fn prioritize_one_account() {
        let account = Account::from(1);
        let ledger = Ledger::new_null_builder()
            .account_info(
                &account,
                &AccountInfo {
                    head: BlockHash::from(2),
                    ..Default::default()
                },
            )
            .finish();
        let any = ledger.any();
        let stats = Stats::default();
        let bootstrap_queue = Arc::new(BootstrapQueue::new_null());
        let state = Mutex::new(BootstrapLogic::new(
            Default::default(),
            bootstrap_queue.clone(),
        ));
        let mut worker = FrontierWorker::new(&any, &stats, &state, &bootstrap_queue);

        worker.process(vec![Frontier::new(account, BlockHash::from(3))]);

        let guard = state.lock().unwrap();
        assert_eq!(bootstrap_queue.info().download_queue, 1);
        assert_eq!(bootstrap_queue.priority(&account), Priority::CUTOFF);
        assert_eq!(guard.frontiers_processor.stats.outdated_accounts_found, 1);
        assert_eq!(guard.frontiers_processor.stats.processed_frontiers, 1);
    }
}
