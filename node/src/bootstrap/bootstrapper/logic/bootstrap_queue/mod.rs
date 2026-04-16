mod blocked;
mod bootstrapping_account;
mod download_queue;
mod downloading;
mod logic;
mod priority;
mod single_block_account_set;

pub use logic::{
    BootstrapQueueConfig, BootstrapQueueInfo, BootstrapQueueSnapshot, BootstrapTarget,
    BootstrappingAccountInfo, PriorityDownResult, PrioritySetResult,
};
pub use priority::Priority;

use logic::BootstrapQueueLogic;

use std::{collections::VecDeque, sync::Mutex};

use rsnano_nullable_clock::SteadyClock;
#[cfg(test)]
use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, Block, BlockHash};
use rsnano_utils::container_info::ContainerInfo;

use bootstrapping_account::AccountState;

pub(crate) struct BootstrapQueue {
    logic: Mutex<BootstrapQueueLogic>,
    clock: SteadyClock,
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
        }
    }

    pub fn account_state(&self, account: &Account) -> Option<AccountState> {
        self.logic.lock().unwrap().account_state(account)
    }

    pub fn priority_set(&mut self, account: &Account, priority: Priority) -> PrioritySetResult {
        self.logic.lock().unwrap().priority_set(account, priority)
    }

    pub fn priority_up(&mut self, account: &Account) -> PrioritySetResult {
        self.logic.lock().unwrap().priority_up(account)
    }

    pub fn priority_down(&mut self, account: &Account) -> PriorityDownResult {
        self.logic.lock().unwrap().priority_down(account)
    }

    #[cfg(test)]
    pub fn priority(&self, account: &Account) -> Priority {
        self.logic.lock().unwrap().priority(account)
    }

    pub fn remove(&mut self, account: &Account) -> bool {
        self.logic.lock().unwrap().remove(account)
    }

    pub fn block(&mut self, account: Account, dependency: BlockHash) -> bool {
        let now = self.clock.now();
        self.logic.lock().unwrap().block(account, dependency, now)
    }

    #[cfg(test)]
    pub fn blocked(&self, account: &Account) -> bool {
        self.logic.lock().unwrap().blocked(account)
    }

    pub fn unblock(&mut self, account: Account, dependency: Option<BlockHash>) -> bool {
        self.logic.lock().unwrap().unblock(account, dependency)
    }

    /// Should be called periodically to remove old entries from the blocked accounts
    pub fn decay_blocked_accounts(&mut self) -> usize {
        let now = self.clock.now();
        self.logic.lock().unwrap().decay_blocked_accounts(now)
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
    pub fn dependency_update(
        &mut self,
        dependency: &BlockHash,
        dependency_account: Account,
    ) -> usize {
        self.logic
            .lock()
            .unwrap()
            .dependency_update(dependency, dependency_account)
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
        self.logic.lock().unwrap().download_started(account, now);
    }

    pub fn download_finished(&mut self, account: &Account, blocks: VecDeque<Block>) {
        self.logic
            .lock()
            .unwrap()
            .download_finished(account, blocks);
    }

    pub fn processing_started(&mut self, block_hash: &BlockHash) -> bool {
        self.logic.lock().unwrap().processing_started(block_hash)
    }

    pub fn reprocess(&mut self, account: &Account, block_hash: &BlockHash) {
        self.logic.lock().unwrap().reprocess(account, block_hash);
    }

    pub fn processing_finished(&mut self, block_hash: &BlockHash) -> Option<AccountState> {
        self.logic.lock().unwrap().processing_finished(block_hash)
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

    pub fn container_info(&self) -> ContainerInfo {
        self.logic.lock().unwrap().container_info()
    }
}

impl Default for BootstrapQueue {
    fn default() -> Self {
        Self::new(Default::default())
    }
}
