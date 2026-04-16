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

pub(crate) use logic::BootstrapQueue;

use std::collections::VecDeque;

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, Block, BlockHash};
use rsnano_utils::container_info::ContainerInfo;

use bootstrapping_account::AccountState;

pub(crate) struct BootstrapQueueService {
    logic: BootstrapQueue,
}

impl BootstrapQueueService {
    pub fn new(config: BootstrapQueueConfig) -> Self {
        Self {
            logic: BootstrapQueue::new(config),
        }
    }

    pub fn account_state(&self, account: &Account) -> Option<AccountState> {
        self.logic.account_state(account)
    }

    pub fn priority_set(&mut self, account: &Account, priority: Priority) -> PrioritySetResult {
        self.logic.priority_set(account, priority)
    }

    pub fn priority_up(&mut self, account: &Account) -> PrioritySetResult {
        self.logic.priority_up(account)
    }

    pub fn priority_down(&mut self, account: &Account) -> PriorityDownResult {
        self.logic.priority_down(account)
    }

    pub fn remove(&mut self, account: &Account) -> bool {
        self.logic.remove(account)
    }

    pub fn block(&mut self, account: Account, dependency: BlockHash, now: Timestamp) -> bool {
        self.logic.block(account, dependency, now)
    }

    pub fn unblock(&mut self, account: Account, dependency: Option<BlockHash>) -> bool {
        self.logic.unblock(account, dependency)
    }

    /// Should be called periodically to remove old entries from the blocked accounts
    pub fn decay_blocked_accounts(&mut self, now: Timestamp) -> usize {
        self.logic.decay_blocked_accounts(now)
    }

    pub fn set_last_request(&mut self, account: &Account, now: Timestamp) {
        self.logic.set_last_request(account, now);
    }

    /// Sets information about the account chain that contains the block hash
    pub fn dependency_update(
        &mut self,
        dependency: &BlockHash,
        dependency_account: Account,
    ) -> usize {
        self.logic.dependency_update(dependency, dependency_account)
    }

    pub fn next_download_target(
        &self,
        now: Timestamp,
        filter: impl Fn(&Account) -> bool,
    ) -> BootstrapTarget {
        self.logic.next_download_target(now, filter)
    }

    pub fn next_block_to_process(&self) -> Option<&Block> {
        self.logic.next_block_to_process()
    }

    pub fn download_started(&mut self, account: &Account, now: Timestamp) {
        self.logic.download_started(account, now);
    }

    pub fn download_finished(&mut self, account: &Account, blocks: VecDeque<Block>) {
        self.logic.download_finished(account, blocks);
    }

    pub fn processing_started(&mut self, block_hash: &BlockHash) -> bool {
        self.logic.processing_started(block_hash)
    }

    pub fn reprocess(&mut self, account: &Account, block_hash: &BlockHash) {
        self.logic.reprocess(account, block_hash);
    }

    pub fn processing_finished(&mut self, block_hash: &BlockHash) -> Option<AccountState> {
        self.logic.processing_finished(block_hash)
    }

    pub fn next_blocked(&self, filter: impl Fn(&BlockHash) -> bool) -> BlockHash {
        self.logic.next_blocked(filter)
    }

    /// Sets information about the account chain that contains the block hash
    /// Returns the number of inserted accounts
    pub fn sync_dependencies(&mut self) -> usize {
        self.logic.sync_dependencies()
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.logic.contains(account)
    }

    pub fn snapshot(&self, limit: usize, filter: Option<Account>) -> BootstrapQueueSnapshot {
        self.logic.snapshot(limit, filter)
    }

    pub fn info(&self) -> BootstrapQueueInfo {
        self.logic.info()
    }

    pub fn queue_half_full(&self) -> bool {
        self.logic.queue_half_full()
    }

    pub fn blocked_half_full(&self) -> bool {
        self.logic.blocked_half_full()
    }

    pub fn clear_blocked_accounts(&mut self) {
        self.logic.clear_blocked_accounts();
    }

    pub fn timeout(&mut self, now: Timestamp) {
        self.logic.timeout(now);
    }

    pub fn revision(&self) -> u64 {
        self.logic.revision()
    }

    pub fn container_info(&self) -> ContainerInfo {
        self.logic.container_info()
    }
}
