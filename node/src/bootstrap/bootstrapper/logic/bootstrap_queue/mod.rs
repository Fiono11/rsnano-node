mod block_processing_queue;
mod blocked;
mod bootstrapping_account;
mod download_queue;
mod downloading;
mod logic;
mod priority;
mod single_block_account_set;
mod stats;

pub use logic::{
    BootstrapQueueConfig, BootstrapQueueInfo, BootstrapQueueSnapshot, BootstrapTarget,
    BootstrappingAccountInfo, PriorityDownResult, PriorityUpResult,
};
pub use priority::Priority;

use logic::BootstrapQueueLogic;

use std::{
    collections::VecDeque,
    sync::{Mutex, atomic::Ordering::Relaxed},
};

use rsnano_nullable_clock::SteadyClock;
#[cfg(test)]
use rsnano_nullable_clock::Timestamp;
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

    pub fn priority_up_to(&mut self, account: &Account, new_priority: Priority) {
        let result = self
            .logic
            .lock()
            .unwrap()
            .priority_up_to(account, new_priority);
        self.stats.add_prio_set_result(&result);
    }

    pub fn priority_up(&mut self, account: &Account) {
        let result = self.logic.lock().unwrap().priority_up(account);
        self.stats.add_prio_set_result(&result);
    }

    pub fn priority_down(&mut self, account: &Account) {
        let result = self.logic.lock().unwrap().priority_down(account);
        self.stats.add_prio_down_result(&result);
    }

    #[cfg(test)]
    pub fn priority(&self, account: &Account) -> Priority {
        self.logic.lock().unwrap().priority(account)
    }

    pub fn remove(&mut self, account: &Account) {
        let removed = self.logic.lock().unwrap().remove(account);
        if removed {
            self.stats.removed.fetch_add(1, Relaxed);
        } else {
            self.stats.remove_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn block(&mut self, account: Account, dependency: BlockHash) {
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

    pub fn unblock(&mut self, account: Account) {
        let unblocked = self.logic.lock().unwrap().unblock(account);
        if unblocked {
            self.stats.unblocked.fetch_add(1, Relaxed);
        }
    }

    /// Should be called periodically to remove old entries from the blocked accounts
    pub fn decay_blocked_accounts(&mut self) {
        let now = self.clock.now();
        let decayed = self.logic.lock().unwrap().decay_blocked_accounts(now);
        self.stats
            .decayed_blocked
            .fetch_add(decayed as u64, Relaxed);
    }

    pub fn set_last_request(&mut self, account: &Account) {
        let now = self.clock.now();
        self.logic.lock().unwrap().set_last_request(account, now);
    }

    #[cfg(test)]
    pub fn last_request(&self, account: &Account) -> Option<Timestamp> {
        self.logic.lock().unwrap().last_request(account)
    }

    /// Sets information about the account chain that contains the block hash
    pub fn dependency_update(&mut self, dependency: &BlockHash, dependency_account: Account) {
        let mut logic = self.logic.lock().unwrap();
        let updated = logic.dependency_update(dependency, dependency_account);
        self.stats
            .dependency_update
            .fetch_add(updated as u64, Relaxed);
        if updated > 0 {
            let result = logic.priority_up(&dependency_account);
            self.stats.add_prio_set_result(&result);
        }
    }

    pub fn next_download_target(&self, filter: impl Fn(&Account) -> bool) -> BootstrapTarget {
        let now = self.clock.now();
        self.logic.lock().unwrap().next_download_target(now, filter)
    }

    pub fn next_block_to_process(&self) -> Option<Block> {
        self.logic.lock().unwrap().next_block_to_process().cloned()
    }

    pub fn download_started(&mut self, account: &Account) {
        let now = self.clock.now();
        let started = self.logic.lock().unwrap().download_started(account, now);
        if started {
            self.stats.download_started.fetch_add(1, Relaxed);
        } else {
            self.stats.download_start_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn download_finished(&mut self, account: &Account, blocks: VecDeque<Block>) {
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

    pub fn processing_started(&mut self, block_hash: &BlockHash) {
        if self.logic.lock().unwrap().processing_started(block_hash) {
            self.stats.processing_started.fetch_add(1, Relaxed);
        } else {
            self.stats.processing_started_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn processing_finished(&mut self, block_hash: &BlockHash) {
        let new_state = self.logic.lock().unwrap().processing_finished(block_hash);
        if new_state.is_some() {
            self.stats.processing_finished.fetch_add(1, Relaxed);
        } else {
            self.stats.processing_finished_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn reprocess(&mut self, account: &Account, block_hash: &BlockHash) {
        let reprocessed = self.logic.lock().unwrap().reprocess(account, block_hash);
        if reprocessed {
            self.stats.reprocess.fetch_add(1, Relaxed);
        } else {
            self.stats.reprocess_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn next_blocked(&self, filter: impl Fn(&BlockHash) -> bool) -> BlockHash {
        self.logic.lock().unwrap().next_blocked(filter)
    }

    /// Sets information about the account chain that contains the block hash
    /// Returns the number of inserted accounts
    pub fn sync_dependencies(&mut self) -> usize {
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

    pub fn clear_blocked_accounts(&mut self) {
        self.logic.lock().unwrap().clear_blocked_accounts();
    }

    pub fn timeout(&mut self) {
        let now = self.clock.now();
        self.logic.lock().unwrap().timeout(now);
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
