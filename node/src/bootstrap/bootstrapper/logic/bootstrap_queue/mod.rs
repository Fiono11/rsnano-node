mod account_priority_tracker;

mod block_handoff_queue;
mod blocked;
mod download_queue;
mod downloading;
mod logic;
mod priority;
mod stats;

pub use account_priority_tracker::{PriorityDownResult, PriorityUpResult};
pub use logic::{
    BootstrapQueueConfig, BootstrapQueueInfo, BootstrapQueueSnapshot, BootstrappingAccountInfo,
};
pub use priority::Priority;

use logic::BootstrapQueueLogic;

use std::{
    collections::VecDeque,
    sync::{Mutex, atomic::Ordering::Relaxed},
};

use rsnano_nullable_clock::SteadyClock;
use rsnano_types::{Account, Block, BlockHash};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use stats::BootstrapQueueStats;

pub(crate) struct BootstrapQueue {
    logic: Mutex<BootstrapQueueLogic>,
    clock: SteadyClock,
    stats: BootstrapQueueStats,
}

impl BootstrapQueue {
    pub fn new(config: BootstrapQueueConfig) -> Self {
        Self::new_impl(config, SteadyClock::default())
    }

    #[cfg(test)]
    pub fn new_null() -> Self {
        Self::new_impl(Default::default(), SteadyClock::new_null())
    }

    fn new_impl(config: BootstrapQueueConfig, clock: SteadyClock) -> Self {
        Self {
            logic: Mutex::new(BootstrapQueueLogic::new(config)),
            clock,
            stats: Default::default(),
        }
    }

    pub fn priority_up_to(&self, account: &Account, new_priority: Priority) {
        let result = self
            .logic
            .lock()
            .unwrap()
            .priority_up_to(account, new_priority);
        self.stats.add_prio_set_result(&result);
    }

    pub fn priority_up(&self, account: &Account) {
        let result = self.logic.lock().unwrap().priority_up(account);
        self.stats.add_prio_set_result(&result);
    }

    pub fn priority_down(&self, account: &Account) {
        let result = self.logic.lock().unwrap().priority_down(account);
        self.stats.add_prio_down_result(&result);
    }

    #[cfg(test)]
    pub fn priority(&self, account: &Account) -> Priority {
        self.logic.lock().unwrap().priority(account)
    }

    pub fn remove(&self, account: &Account) {
        let removed = self.logic.lock().unwrap().remove(account);
        if removed {
            self.stats.removed.fetch_add(1, Relaxed);
        } else {
            self.stats.remove_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn block(&self, account: Account, dependency: BlockHash) {
        let now = self.clock.now();
        let blocked = self.logic.lock().unwrap().block(account, dependency, now);
        if blocked {
            self.stats.blocked.fetch_add(1, Relaxed);
        } else {
            self.stats.block_failed.fetch_add(1, Relaxed);
        }
    }

    #[cfg(test)]
    pub fn blocked(&self, account: &Account) -> bool {
        self.logic.lock().unwrap().blocked(account)
    }

    pub fn unblock(&self, account: Account) {
        let unblocked = self.logic.lock().unwrap().unblock(account);
        if unblocked {
            self.stats.unblocked.fetch_add(1, Relaxed);
        }
    }

    pub fn dependency_account_requested(&self, dependency: &BlockHash) {
        let now = self.clock.now();
        self.logic
            .lock()
            .unwrap()
            .dependency_account_requested(dependency, now);
        self.stats.dependency_requested.fetch_add(1, Relaxed);
    }

    pub fn dependency_account_not_found(&self, dependency: &BlockHash) {
        self.logic
            .lock()
            .unwrap()
            .dependency_account_not_found(dependency);
    }

    /// Sets information about the account chain that contains the block hash
    pub fn dependency_update(&self, dependency: &BlockHash, dependency_account: Account) {
        let mut logic = self.logic.lock().unwrap();
        let (updated, prio_result) = logic.dependency_update(dependency, dependency_account);
        if updated > 0 {
            self.stats
                .dependency_update
                .fetch_add(updated as u64, Relaxed);
            self.stats.add_prio_set_result(&prio_result);
        }
    }

    pub fn next_download_target(&self) -> Option<(Account, Priority)> {
        self.logic.lock().unwrap().next_download_target()
    }

    pub fn next_block_to_process(&self) -> Option<Block> {
        self.logic.lock().unwrap().next_block_to_process().cloned()
    }

    pub fn download_started(&self, account: &Account) {
        let now = self.clock.now();
        let started = self.logic.lock().unwrap().download_started(account, now);
        if started {
            self.stats.download_started.fetch_add(1, Relaxed);
        } else {
            self.stats.download_start_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn download_finished(&self, account: &Account, blocks: VecDeque<Block>) {
        let finished = self
            .logic
            .lock()
            .unwrap()
            .download_finished(account, blocks);
        if finished {
            self.stats.download_finished.fetch_add(1, Relaxed);
        } else {
            self.stats.download_finished_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn processing_started(&self, block_hash: &BlockHash) {
        if self.logic.lock().unwrap().processing_started(block_hash) {
            self.stats.processing_started.fetch_add(1, Relaxed);
        } else {
            self.stats.processing_started_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn processing_finished(&self, block_hash: &BlockHash) {
        let finished = self.logic.lock().unwrap().processing_finished(block_hash);
        if finished {
            self.stats.processing_finished.fetch_add(1, Relaxed);
        } else {
            self.stats.processing_finished_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn reprocess(&self, account: &Account, block_hash: &BlockHash) {
        let reprocessed = self.logic.lock().unwrap().reprocess(account, block_hash);
        if reprocessed {
            self.stats.reprocess.fetch_add(1, Relaxed);
        } else {
            self.stats.reprocess_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn next_unknown_blocking_hash(&self, filter: impl Fn(&BlockHash) -> bool) -> BlockHash {
        self.logic
            .lock()
            .unwrap()
            .next_unknown_blocking_hash(filter)
    }

    /// Sets information about the account chain that contains the block hash
    /// Returns the number of inserted accounts
    pub fn sync_dependencies(&self) -> usize {
        self.logic.lock().unwrap().sync_dependencies()
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.logic.lock().unwrap().contains(account)
    }

    pub fn snapshot(&self, limit: usize, filter: Option<Account>) -> BootstrapQueueSnapshot {
        self.logic.lock().unwrap().snapshot(limit, filter)
    }

    pub fn info(&self) -> BootstrapQueueInfo {
        self.logic.lock().unwrap().info()
    }

    pub fn queue_half_full(&self) -> bool {
        self.logic.lock().unwrap().queue_half_full()
    }

    pub fn blocked_half_full(&self) -> bool {
        self.logic.lock().unwrap().blocked_half_full()
    }

    pub fn clear_blocked_accounts(&self) {
        self.logic.lock().unwrap().clear_blocked_accounts();
    }

    pub fn missing_sends(&self) -> Vec<BlockHash> {
        self.logic.lock().unwrap().missing_sends()
    }

    pub fn processing(&self) -> Vec<BlockHash> {
        self.logic.lock().unwrap().processing()
    }

    pub fn timeout(&self) {
        let now = self.clock.now();
        let decayed = self.logic.lock().unwrap().timeout(now);
        self.stats
            .decayed_blocked
            .fetch_add(decayed as u64, Relaxed);
    }

    pub fn revision(&self) -> u64 {
        self.logic.lock().unwrap().revision()
    }
}

impl Default for BootstrapQueue {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl ContainerInfoProvider for BootstrapQueue {
    fn container_info(&self) -> ContainerInfo {
        self.logic.lock().unwrap().container_info()
    }
}

impl StatsSource for BootstrapQueue {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result);
    }
}
