mod blocked;
mod download_queue;
mod downloading;
mod priority;
mod single_block_account_set;

use std::{cmp::min, collections::VecDeque, time::Duration};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, Block, BlockHash};
use rsnano_utils::container_info::ContainerInfo;

use crate::bootstrap::bootstrapper::state::{
    block_queue::BlockInfo,
    bootstrap_queue::{
        downloading::DownloadingAccounts, single_block_account_set::SingleBlockAccountSet,
    },
};
use blocked::BlockedAccounts;
use download_queue::{ChangePriorityResult, DownloadQueue};
pub use priority::Priority;
use rustc_hash::FxHashMap;

/// An account that is currently being bootstrapped
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct BootstrappingAccount {
    pub account: Account,
    pub priority: Priority,
    pub fails: usize,
    pub last_request: Option<Timestamp>,
    pub blocked: Option<BlockedInfo>,
    pub blocks: VecDeque<Block>,
    pub state: AccountState,
}

impl BootstrappingAccount {
    pub fn new(account: Account, priority: Priority) -> Self {
        Self {
            account,
            priority,
            fails: 0,
            last_request: None,
            blocked: None,
            blocks: VecDeque::new(),
            state: AccountState::EnqueuedForDownload,
        }
    }

    #[cfg(test)]
    pub fn new_test_instance() -> Self {
        Self {
            account: Account::from(7),
            priority: Priority::new(3.0),
            fails: 0,
            last_request: None,
            blocked: None,
            blocks: VecDeque::new(),
            state: AccountState::EnqueuedForDownload,
        }
    }

    #[cfg(test)]
    pub fn new_blocked_test_instance() -> Self {
        Self {
            account: Account::from(7),
            priority: Priority::new(3.0),
            fails: 0,
            last_request: None,
            blocked: Some(BlockedInfo::new_test_instance()),
            blocks: VecDeque::new(),
            state: AccountState::Blocked,
        }
    }

    pub fn first_block_hash(&self) -> Option<BlockHash> {
        let front = self.blocks.front()?;
        Some(front.hash())
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub(crate) enum AccountState {
    #[default]
    EnqueuedForDownload,
    Downloading,
    ReadyToProcess,
    Processing,
    Blocked,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockedInfo {
    pub dependency_block: BlockHash,
    /// Account that contains the dependency block, fetched via a background dependency walker
    pub dependency_account: Option<Account>,
    pub blocked_at: Timestamp,
}

impl BlockedInfo {
    pub fn new_test_instance() -> Self {
        Self {
            dependency_block: BlockHash::from(456),
            dependency_account: None,
            blocked_at: Timestamp::new_test_instance(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BootstrapQueueConfig {
    pub max_unblocked_accounts: usize,
    pub max_blocked_accounts: usize,

    /// A blocked account is removed if it has been blocked for blocked_decay
    pub blocked_decay: Duration,

    /// After a request was made for an account, the account goes into cooldown,
    /// so that we don't immediately create more requests for it, because we need
    /// to wait a bit for the responses to come in
    pub account_cooldown: Duration,
}

impl Default for BootstrapQueueConfig {
    fn default() -> Self {
        Self {
            max_unblocked_accounts: 256 * 1024,
            max_blocked_accounts: 256 * 1024,
            blocked_decay: Duration::from_hours(1),
            account_cooldown: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PriorityUpResult {
    Inserted,
    Updated,
    AccountBlocked,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PriorityDownResult {
    Deprioritized,
    /// The priority got too low, so the account got erased
    Erased,
    AccountNotFound,
    InvalidAccount,
}

/// A prioritized queue of accounts which should bootstrapped.
/// Accounts can be blocked, because a dependency block is missing. Blocked accounts
/// are put on hold.
pub struct BootstrapQueue {
    config: BootstrapQueueConfig,
    accounts: FxHashMap<Account, BootstrappingAccount>,
    download_queue: DownloadQueue,
    downloading: DownloadingAccounts,
    ready_to_process: SingleBlockAccountSet,
    processing: SingleBlockAccountSet,
    blocked: BlockedAccounts,
    revision: u64,
}

impl BootstrapQueue {
    pub const MAX_FAILS: usize = 3;

    pub fn new(config: BootstrapQueueConfig) -> Self {
        Self {
            config,
            accounts: Default::default(),
            download_queue: Default::default(),
            blocked: Default::default(),
            downloading: Default::default(),
            ready_to_process: Default::default(),
            processing: Default::default(),
            revision: 0,
        }
    }

    pub fn priority_set(&mut self, account: &Account, priority: Priority) -> bool {
        let result = self.modify_priority(account, |e| {
            e.priority = priority;
            e.fails = 0;
        });
        self.revision += 1;

        if result == ChangePriorityResult::NotFound {
            self.accounts
                .insert(*account, BootstrappingAccount::new(*account, priority));
            self.download_queue.insert(*account, priority);
            self.trim_overflow();
        }

        self.revision += 1;
        true
    }

    pub fn priority_up(&mut self, account: &Account) -> PriorityUpResult {
        if account.is_zero() {
            return PriorityUpResult::AccountBlocked;
        }

        let result = self.modify_priority(account, |e| {
            e.priority = Self::higher_priority(e.priority);
            e.fails = 0;
        });
        self.revision += 1;

        if result == ChangePriorityResult::NotFound {
            self.accounts.insert(
                *account,
                BootstrappingAccount::new(*account, Priority::INITIAL),
            );
            self.download_queue.insert(*account, Priority::INITIAL);
            self.trim_overflow();
            PriorityUpResult::Inserted
        } else {
            PriorityUpResult::Updated
        }
    }

    fn higher_priority(priority: Priority) -> Priority {
        min(priority + Priority::INCREASE, Priority::MAX)
    }

    pub fn priority_down(&mut self, account: &Account) -> PriorityDownResult {
        if account.is_zero() {
            return PriorityDownResult::InvalidAccount;
        }

        let change_result = self.modify_priority(account, |e| {
            e.priority = e.priority / Priority::DIVIDE;
            e.fails += 1;
        });

        self.revision += 1;
        match change_result {
            ChangePriorityResult::Updated => PriorityDownResult::Deprioritized,
            ChangePriorityResult::Deleted => PriorityDownResult::Erased,
            ChangePriorityResult::NotFound => PriorityDownResult::AccountNotFound,
        }
    }

    fn modify_priority<F>(&mut self, account: &Account, f: F) -> ChangePriorityResult
    where
        F: Fn(&mut BootstrappingAccount),
    {
        let Some(entry) = self.accounts.get_mut(account) else {
            return ChangePriorityResult::NotFound;
        };
        let old_prio = entry.priority;
        f(entry);

        if entry.fails >= BootstrapQueue::MAX_FAILS
            || entry.fails as f64 >= entry.priority.as_f64()
            || entry.priority <= Priority::CUTOFF
        {
            self.remove(account);
            return ChangePriorityResult::Deleted;
        }

        if entry.priority != old_prio && entry.state == AccountState::EnqueuedForDownload {
            self.download_queue
                .change_priority(account, old_prio, entry.priority);
        }
        ChangePriorityResult::Updated
    }

    pub fn remove(&mut self, account: &Account) -> bool {
        if account.is_zero() {
            return false;
        }

        let mut to_remove = VecDeque::new();
        to_remove.push_back(*account);
        while let Some(account) = to_remove.pop_front() {
            let Some(removed) = self.accounts.remove(&account) else {
                return false;
            };
            match removed.state {
                AccountState::EnqueuedForDownload => {
                    self.download_queue.remove(&account, removed.priority)
                }
                AccountState::Downloading => {
                    self.downloading.remove(&account);
                }
                AccountState::ReadyToProcess => {
                    self.ready_to_process
                        .remove_block(&removed.first_block_hash().unwrap());
                }
                AccountState::Processing => {
                    self.processing
                        .remove_block(&removed.first_block_hash().unwrap());
                }
                AccountState::Blocked => {
                    to_remove.extend(self.blocked.remove_account_and_dependents(&account));
                }
            }
        }
        self.revision += 1;
        true
    }

    pub fn block(&mut self, account: Account, dependency: BlockHash, now: Timestamp) -> bool {
        debug_assert!(!account.is_zero());

        let Some(entry) = self.accounts.get_mut(&account) else {
            return false;
        };
        match entry.state {
            AccountState::EnqueuedForDownload => {
                self.download_queue.remove(&account, entry.priority)
            }
            AccountState::Downloading => {
                self.downloading.remove(&account);
            }
            AccountState::ReadyToProcess => {
                self.ready_to_process
                    .remove_block(&entry.first_block_hash().unwrap());
            }
            AccountState::Processing => {
                self.processing
                    .remove_block(&entry.first_block_hash().unwrap());
            }
            AccountState::Blocked => return true,
        }

        entry.blocked = Some(BlockedInfo {
            dependency_block: dependency,
            dependency_account: None,
            blocked_at: now,
        });
        entry.state = AccountState::Blocked;
        self.blocked.insert(account, dependency, now);
        self.trim_overflow();
        self.revision += 1;
        true
    }

    pub fn unblock(&mut self, account: Account, dependency: Option<BlockHash>) -> bool {
        let Some(entry) = self.accounts.get_mut(&account) else {
            return false;
        };
        if entry.state != AccountState::Blocked {
            return false;
        }
        if let Some(expected_dependency) = dependency {
            // Unblock only if the given dependency is fulfilled
            if expected_dependency != entry.blocked.as_ref().unwrap().dependency_block {
                // Not the dependency we were looking for...
                return false;
            }
        }

        self.blocked.remove(&account);
        entry.blocked = None;

        if !entry.blocks.is_empty() {
            entry.state = AccountState::ReadyToProcess;
            self.ready_to_process
                .insert(entry.account, entry.first_block_hash().unwrap());
        } else {
            entry.state = AccountState::EnqueuedForDownload;
            self.download_queue.insert(account, entry.priority)
        }

        self.trim_overflow();
        self.revision += 1;
        true
    }

    /// Should be called periodically to remove old entries from the blocked accounts
    pub fn decay_blocked_accounts(&mut self, now: Timestamp) -> usize {
        let cutoff = now - self.config.blocked_decay;
        self.revision += 1;
        let removed = self.blocked.remove_older_than(cutoff);
        // TODO remove from account
        removed.len()
    }

    #[cfg(test)]
    pub fn last_request(&self, account: &Account) -> Option<Timestamp> {
        self.accounts.get(account)?.last_request
    }

    pub fn set_last_request(&mut self, account: &Account, now: Timestamp) {
        debug_assert!(!account.is_zero());
        if let Some(entry) = self.accounts.get_mut(account) {
            entry.last_request = Some(now);
            self.revision += 1;
        }
    }

    pub fn reset_last_request(&mut self, account: &Account) {
        debug_assert!(!account.is_zero());
        if let Some(entry) = self.accounts.get_mut(account) {
            entry.last_request = None;
            self.revision += 1;
        }
    }

    /// Sets information about the account chain that contains the block hash
    pub fn dependency_update(
        &mut self,
        dependency: &BlockHash,
        dependency_account: Account,
    ) -> usize {
        debug_assert!(!dependency_account.is_zero());
        let updated = self
            .blocked
            .modify_dependency_account(dependency, dependency_account);

        if updated > 0 && !self.queue_full() {
            self.priority_up(&dependency_account);
        }

        updated
    }

    /// Erase the oldest entries
    fn trim_overflow(&mut self) {
        while self.needs_trimming() {
            let account = self.download_queue.pop_lowest_prio().unwrap();
            self.accounts.remove(&account);
        }
        while self.blocked.len() > self.config.max_blocked_accounts {
            self.blocked.remove_oldest();
        }
    }

    fn needs_trimming(&self) -> bool {
        !self.download_queue.is_empty()
            && self.unblocked_count() > self.config.max_unblocked_accounts
    }

    pub fn next_download_target(
        &self,
        now: Timestamp,
        filter: impl Fn(&Account) -> bool,
    ) -> BootstrapTarget {
        if self.download_queue.is_empty() {
            return Default::default();
        }

        let cutoff = now - self.config.account_cooldown;

        let target = self.download_queue.iter().find_map(|(prio, account)| {
            let entry = self.accounts.get(account).unwrap();
            let is_match = if let Some(last) = &entry.last_request {
                if *last > cutoff {
                    false
                } else {
                    filter(account)
                }
            } else {
                true
            };

            if is_match {
                Some(BootstrapTarget {
                    account: *account,
                    priority: prio,
                    fails: entry.fails,
                })
            } else {
                None
            }
        });

        target.unwrap_or_default()
    }

    pub fn next_block_to_process(&self) -> Option<&Block> {
        let account = self.ready_to_process.next_account()?;
        self.accounts.get(&account).unwrap().blocks.front()
    }

    pub fn download_started(&mut self, account: &Account, now: Timestamp) {
        let Some(entry) = self.accounts.get_mut(&account) else {
            return;
        };
        if entry.state != AccountState::EnqueuedForDownload {
            return;
        }
        self.download_queue.remove(account, entry.priority);
        entry.state = AccountState::Downloading;
        self.downloading.insert(*account, now);
    }

    pub fn download_finished(&mut self, account: &Account, blocks: VecDeque<Block>) {
        let Some(entry) = self.accounts.get_mut(account) else {
            return;
        };
        if entry.state != AccountState::Downloading {
            return;
        }
        self.downloading.remove(account);
        entry.blocks = blocks;
        if entry.blocks.is_empty() {
            entry.state = AccountState::EnqueuedForDownload;
            self.download_queue.insert(*account, entry.priority);
        } else {
            entry.state = AccountState::ReadyToProcess;
            self.ready_to_process
                .insert(*account, entry.first_block_hash().unwrap());
        }
    }

    pub fn processing_started(&mut self, block_hash: &BlockHash) {
        if let Some(account) = self.ready_to_process.remove_block(block_hash) {
            let entry = self.accounts.get_mut(&account).unwrap();
            entry.state = AccountState::Processing;
            self.processing.insert(account, *block_hash);
        } else {
            // TODO
        }
    }

    pub(crate) fn processing_finished(&mut self, block_hash: &BlockHash) -> BlockInfo {
        if let Some(account) = self.processing.remove_block(block_hash) {
            let entry = self.accounts.get_mut(&account).unwrap();
            let first_block = entry.blocks.pop_front().unwrap();
            assert_eq!(first_block.hash(), *block_hash);
            if entry.blocks.is_empty() {
                let block_info = BlockInfo {
                    account: Some(entry.account),
                    was_last: true,
                };
                entry.state = AccountState::EnqueuedForDownload;
                self.download_queue.insert(account, entry.priority);
                return block_info;
            } else {
                let block_info = BlockInfo {
                    account: Some(entry.account),
                    was_last: false,
                };
                entry.state = AccountState::ReadyToProcess;
                self.ready_to_process
                    .insert(account, entry.first_block_hash().unwrap());
                return block_info;
            }
        } else {
            // TODO
        }

        BlockInfo {
            account: None,
            was_last: false,
        }
    }

    pub fn next_blocked(&self, filter: impl Fn(&BlockHash) -> bool) -> BlockHash {
        if self.blocked.is_empty() {
            return BlockHash::ZERO;
        }

        self.blocked.next(filter).unwrap_or_default()
    }

    /// Sets information about the account chain that contains the block hash
    /// Returns the number of inserted accounts
    pub fn sync_dependencies(&mut self) -> usize {
        if self.queue_full() {
            return 0;
        }

        let mut inserted = 0;

        let mut accounts_to_move = Vec::new();
        // Sample all accounts with a known dependency account (> account 0)
        let begin = Account::from(1);
        for account in self.blocked.iter_start_dep_account(begin) {
            if self.queue_full() {
                break;
            }

            let entry = self.accounts.get(account).unwrap();
            let dep_account = entry
                .blocked
                .as_ref()
                .unwrap()
                .dependency_account
                .unwrap_or_default();

            if !self.contains(&dep_account) {
                accounts_to_move.push(dep_account);
            }
        }

        for account in accounts_to_move {
            if self.queue_full() {
                break;
            }

            if self.unblock(account, None) {
                inserted += 1;
            }
        }

        inserted
    }

    pub fn blocked(&self, account: &Account) -> bool {
        self.blocked.contains(account)
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.accounts.contains_key(account)
    }

    pub fn unblocked_count(&self) -> usize {
        self.accounts.len() - self.blocked.len()
    }

    pub fn download_queue_len(&self) -> usize {
        self.download_queue.len()
    }

    pub fn blocked_count(&self) -> usize {
        self.blocked.len()
    }

    pub fn downloading_count(&self) -> usize {
        self.downloading.len()
    }

    pub fn ready_to_process_count(&self) -> usize {
        self.ready_to_process.len()
    }

    pub fn unique_blocked_accounts(&self) -> usize {
        self.blocked.unique_dependency_accounts()
    }

    pub fn known_dependencies(&self) -> usize {
        self.blocked.known_dependencies()
    }

    pub fn iter_priorities(&self) -> impl Iterator<Item = (Priority, &Account)> {
        self.download_queue.iter()
    }

    pub fn iter_blocked(&self) -> impl Iterator<Item = (Account, BlockHash, Account)> {
        self.blocked.iter_by_insertion_order().map(|account| {
            let entry = self.accounts.get(account).unwrap();
            let blocked = entry.blocked.as_ref().unwrap();
            (
                *account,
                blocked.dependency_block,
                blocked.dependency_account.unwrap_or_default(),
            )
        })
    }

    pub fn queue_full(&self) -> bool {
        self.unblocked_count() >= self.config.max_unblocked_accounts
    }

    pub fn queue_half_full(&self) -> bool {
        self.unblocked_count() > self.config.max_unblocked_accounts / 2
    }

    pub fn blocked_half_full(&self) -> bool {
        self.blocked.len() > self.config.max_blocked_accounts / 2
    }

    /// Accounts in the ledger but not in priority list are assumed priority 1.0f
    /// Blocked accounts are assumed priority 0.0f
    #[allow(dead_code)]
    pub fn priority(&self, account: &Account) -> Priority {
        self.accounts
            .get(account)
            .map(|a| a.priority)
            .unwrap_or(Priority::ZERO)
    }

    pub fn clear_blocked_accounts(&mut self) {
        self.blocked.clear();
        self.revision += 1;
    }

    pub fn timeout(&mut self, now: Timestamp) {
        while let Some(account) = self.downloading.pop_timeout(now) {
            let entry = self.accounts.get_mut(&account).unwrap();
            entry.state = AccountState::Downloading;
            self.download_queue.insert(account, entry.priority);
        }
        self.trim_overflow();
        self.revision += 1;
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn container_info(&self) -> ContainerInfo {
        // Count blocked entries with their dependency account unknown
        let blocked_unknown = self.blocked.count_by_dependency_account(&Account::ZERO);
        [
            ("priorities", self.download_queue.len(), 0),
            ("blocked", self.blocked.len(), 0),
            ("blocked_unknown", blocked_unknown, 0),
        ]
        .into()
    }
}

impl Default for BootstrapQueue {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct BootstrapTarget {
    pub account: Account,
    pub priority: Priority,
    pub fails: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_blocked() {
        let queue = BootstrapQueue::default();
        assert_eq!(queue.blocked(&Account::from(1)), false);
    }

    #[test]
    fn block() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);
        queue.priority_up(&account);

        queue.block(account, hash, Timestamp::new_test_instance());

        assert!(queue.blocked(&account));
        assert_eq!(queue.priority(&account), Priority::ZERO);
    }

    #[test]
    fn blocking_unknown_account_does_nothing() {
        let mut queue = BootstrapQueue::default();
        let blocked = queue.block(
            Account::from(1),
            BlockHash::from(2),
            Timestamp::new_test_instance(),
        );
        assert!(!blocked);
        assert_eq!(queue.blocked_count(), 0);
    }

    #[test]
    fn unblock() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);
        queue.priority_up(&account);

        queue.block(account, hash, Timestamp::new_test_instance());

        assert!(queue.unblock(account, None));
        assert_eq!(queue.blocked(&account), false);
    }

    #[test]
    fn unblock_zero_account() {
        let mut queue = BootstrapQueue::default();
        assert!(!queue.unblock(Account::ZERO, None));
    }

    #[test]
    fn unblock_unknown_account() {
        let mut queue = BootstrapQueue::default();
        assert!(!queue.unblock(Account::from(1), None));
    }

    #[test]
    fn unblock_with_unfulfilled_dependency_does_nothing() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);
        queue.priority_set(&account, Priority::INITIAL);
        queue.block(account, hash, Timestamp::new_test_instance());

        let unblocked = queue.unblock(account, Some(BlockHash::from(3)));
        assert!(!unblocked);
        assert!(queue.blocked(&account));
    }

    #[test]
    fn unblock_with_unknown_dependency_does_nothing() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let dependency = BlockHash::from(2);
        let unknown_dependency = BlockHash::from(3);
        queue.priority_set(&account, Priority::INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());

        let unblocked = queue.unblock(account, Some(unknown_dependency));
        assert!(!unblocked);
        assert!(queue.blocked(&account));
    }

    #[test]
    fn priority_base() {
        let queue = BootstrapQueue::default();
        assert_eq!(queue.priority(&Account::from(1)), Priority::ZERO);
    }

    #[test]
    fn priority_unblock() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);

        assert_eq!(queue.priority_up(&account), PriorityUpResult::Inserted);
        assert_eq!(queue.priority(&account), Priority::INITIAL);

        queue.block(account, hash, Timestamp::new_test_instance());
        queue.unblock(account, None);

        assert_eq!(queue.priority(&account), Priority::INITIAL);
    }

    #[test]
    fn priority_up_down() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);

        queue.priority_up(&account);
        assert_eq!(queue.priority(&account), Priority::INITIAL);

        queue.priority_down(&account);
        assert_eq!(
            queue.priority(&account),
            Priority::INITIAL / Priority::DIVIDE
        );
    }

    #[test]
    fn priority_down_empty() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);

        queue.priority_down(&account);

        assert_eq!(queue.priority(&account), Priority::ZERO);
    }

    // Ensure priority value is bounded
    #[test]
    fn saturate_priority() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);

        for _ in 0..100 {
            queue.priority_up(&account);
        }
        assert_eq!(queue.priority(&account), Priority::MAX);
    }

    #[test]
    fn priority_down_saturate() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_up(&account);
        assert_eq!(queue.priority(&account), Priority::INITIAL);
        for _ in 0..10 {
            queue.priority_down(&account);
        }
        assert_eq!(queue.contains(&account), false);
    }

    #[test]
    fn priority_set() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let prio = Priority::new(10.0);
        queue.priority_set(&account, prio);
        assert_eq!(queue.priority(&account), prio);
    }

    #[test]
    fn priority_up_for_zero_account_fails() {
        let mut queue = BootstrapQueue::default();
        let result = queue.priority_up(&Account::ZERO);
        assert_eq!(result, PriorityUpResult::AccountBlocked);
        assert_eq!(queue.blocked_count(), 0);
        assert_eq!(queue.download_queue_len(), 0);
    }

    #[test]
    fn priority_up_for_blocked_account_fails() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, Priority::INITIAL);
        queue.block(account, BlockHash::from(2), Timestamp::new_test_instance());
        let result = queue.priority_up(&account);
        assert_eq!(result, PriorityUpResult::AccountBlocked);
        assert_eq!(queue.blocked_count(), 1);
    }

    #[test]
    fn priority_down_for_zero_account_fails() {
        let mut queue = BootstrapQueue::default();
        let result = queue.priority_down(&Account::ZERO);
        assert_eq!(result, PriorityDownResult::InvalidAccount);
        assert_eq!(queue.blocked_count(), 0);
        assert_eq!(queue.download_queue_len(), 0);
    }

    #[test]
    fn priority_set_for_zero_account_fails() {
        let mut queue = BootstrapQueue::default();
        let success = queue.priority_set(&Account::ZERO, Priority::INITIAL);
        assert_eq!(success, false);
        assert_eq!(queue.blocked_count(), 0);
        assert_eq!(queue.download_queue_len(), 0);
    }

    #[test]
    fn priority_set_fails_for_blocked_account() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, Priority::INITIAL);
        queue.block(account, BlockHash::from(2), Timestamp::new_test_instance());
        let success = queue.priority_set(&account, Priority::new(42.0));

        assert_eq!(success, false);
        assert_eq!(queue.blocked_count(), 1);
        assert_eq!(queue.download_queue_len(), 0);
    }

    #[test]
    fn priority_erase() {
        let mut queue = BootstrapQueue::default();
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        queue.priority_set(&account1, Priority::INITIAL);
        queue.priority_set(&account2, Priority::INITIAL);
        let removed = queue.remove(&account1);
        assert!(removed);
        assert!(!queue.contains(&account1));
        assert!(queue.contains(&account2));
    }

    #[test]
    fn priority_erase_zero_account() {
        let mut queue = BootstrapQueue::default();
        assert!(!queue.remove(&Account::ZERO));
    }

    #[test]
    fn set_last_request_for_unknown_account_does_nothing() {
        let mut queue = BootstrapQueue::default();
        queue.set_last_request(&Account::from(1), Timestamp::new_test_instance());
        assert_eq!(queue.download_queue_len(), 0);
    }

    #[test]
    fn set_last_request() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, Priority::INITIAL);
        let new_timestamp = Timestamp::new_test_instance() + Duration::from_secs(1000);
        queue.set_last_request(&account, new_timestamp);
        assert_eq!(queue.last_request(&account), Some(new_timestamp))
    }

    #[test]
    fn timestamp_reset() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, Priority::INITIAL);
        queue.reset_last_request(&account);
        assert_eq!(queue.last_request(&account), None);
    }

    #[test]
    fn trim_priorities_on_overflow() {
        let mut queue = BootstrapQueue::new(BootstrapQueueConfig {
            max_unblocked_accounts: 2,
            ..Default::default()
        });
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(3);
        queue.priority_set(&account1, Priority::new(2.0));
        queue.priority_set(&account2, Priority::new(1.0));
        queue.priority_set(&account3, Priority::new(3.0));

        assert_eq!(queue.download_queue_len(), 2);
        assert!(queue.contains(&account1));
        assert!(queue.contains(&account3));
        assert!(!queue.contains(&account2));
    }

    #[test]
    fn trim_bocked_on_overflow() {
        let mut queue = BootstrapQueue::new(BootstrapQueueConfig {
            max_blocked_accounts: 2,
            ..Default::default()
        });
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(3);
        queue.priority_up(&account1);
        queue.priority_up(&account2);
        queue.priority_up(&account3);
        queue.block(account1, BlockHash::from(1), Timestamp::new_test_instance());
        queue.block(account2, BlockHash::from(2), Timestamp::new_test_instance());
        queue.block(account3, BlockHash::from(3), Timestamp::new_test_instance());

        assert_eq!(queue.blocked_count(), 2);
        assert!(queue.blocked(&account2));
        assert!(queue.blocked(&account3));
        assert!(!queue.blocked(&account1));
    }

    #[test]
    fn next_priority_empty() {
        let queue = BootstrapQueue::default();
        let next = queue.next_download_target(Timestamp::new_test_instance(), |_| true);
        assert_eq!(next, BootstrapTarget::default());
    }

    #[test]
    fn next_priority() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, Priority::INITIAL);
        let now = Timestamp::new_test_instance();
        let next = queue.next_download_target(now, |_| true);
        assert_eq!(
            next,
            BootstrapTarget {
                account,
                priority: Priority::INITIAL,
                fails: 0
            }
        )
    }

    #[test]
    fn next_priority_none_above_cutoff() {
        let mut queue = BootstrapQueue::default();
        let now = Timestamp::new_test_instance();
        let account = Account::from(1);
        queue.priority_up(&account);
        queue.set_last_request(&account, now);
        let next = queue.next_download_target(now, |_| true);
        assert_eq!(next, BootstrapTarget::default());
    }

    #[test]
    fn next_priority_cutoff() {
        let config = BootstrapQueueConfig::default();
        let mut queue = BootstrapQueue::new(config.clone());
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        queue.priority_set(&account1, Priority::new(100.0));
        queue.priority_set(&account2, Priority::new(1.0));
        let now = Timestamp::new_test_instance();
        queue.set_last_request(
            &account1,
            now - config.account_cooldown + Duration::from_millis(1),
        );
        queue.set_last_request(&account2, now - config.account_cooldown);
        let next = queue.next_download_target(now, |_| true);
        assert_eq!(
            next,
            BootstrapTarget {
                account: account2,
                priority: Priority::new(1.0),
                fails: 0
            }
        );
    }

    #[test]
    fn next_priority_filter() {
        let config = BootstrapQueueConfig::default();
        let mut queue = BootstrapQueue::new(config.clone());
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(2);
        queue.priority_set(&account1, Priority::INITIAL);
        queue.priority_set(&account2, Priority::INITIAL);
        queue.priority_set(&account3, Priority::INITIAL);
        let now = Timestamp::new_test_instance();
        let next = queue.next_download_target(now, |a| *a == account2);
        assert_eq!(
            next,
            BootstrapTarget {
                account: account2,
                priority: Priority::INITIAL,
                fails: 0
            }
        );
    }

    #[test]
    fn next_blocked_empty() {
        let queue = BootstrapQueue::default();
        assert_eq!(queue.next_blocked(|_| true), BlockHash::ZERO);
    }

    #[test]
    fn next_blocked() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let dependency = BlockHash::from(2);
        queue.priority_set(&account, Priority::INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());
        assert_eq!(queue.next_blocked(|_| true), dependency);
    }

    #[test]
    fn next_blocked_filter() {
        let mut queue = BootstrapQueue::default();
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(3);
        let dependency = BlockHash::from(2);
        queue.priority_set(&account1, Priority::INITIAL);
        queue.priority_set(&account2, Priority::INITIAL);
        queue.priority_set(&account3, Priority::INITIAL);
        queue.block(
            account1,
            BlockHash::from(1000),
            Timestamp::new_test_instance(),
        );
        queue.block(account2, dependency, Timestamp::new_test_instance());
        queue.block(
            account3,
            BlockHash::from(2000),
            Timestamp::new_test_instance(),
        );
        assert_eq!(queue.next_blocked(|h| *h == dependency), dependency);
    }

    #[test]
    fn sync_dependencies_empty() {
        let mut queue = BootstrapQueue::default();
        let inserted = queue.sync_dependencies();
        assert_eq!(inserted, 0);
    }

    #[test]
    fn sync_dependencies_insert_one_account() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        queue.priority_set(&account, Priority::INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());

        queue.dependency_update(&dependency, dependency_account);

        assert!(queue.contains(&dependency_account));
    }

    #[test]
    fn sync_dependencies_doesnt_insert_when_dependency_account_already_prioritized() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        queue.priority_set(&account, Priority::INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());
        queue.dependency_update(&dependency, dependency_account);
        queue.priority_set(&dependency_account, Priority::INITIAL);

        let inserted = queue.sync_dependencies();

        assert_eq!(inserted, 0);
    }

    #[test]
    fn sync_dependencies_doesnt_insert_when_max_accounts_prioritized() {
        let config = BootstrapQueueConfig {
            max_unblocked_accounts: 2,
            ..Default::default()
        };
        let mut queue = BootstrapQueue::new(config);
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        queue.priority_set(&account, Priority::INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());
        queue.dependency_update(&dependency, dependency_account);
        queue.priority_set(&Account::from(9999), Priority::INITIAL);
        queue.priority_set(&Account::from(8888), Priority::INITIAL);

        let inserted = queue.sync_dependencies();

        assert_eq!(inserted, 0);
    }

    #[test]
    fn blocked_half_full() {
        let config = BootstrapQueueConfig {
            max_blocked_accounts: 3,
            ..Default::default()
        };
        let mut queue = BootstrapQueue::new(config);
        let account1 = Account::from(1);
        let account2 = Account::from(2);

        assert!(!queue.blocked_half_full());

        queue.priority_set(&account1, Priority::INITIAL);
        queue.block(account1, BlockHash::from(1), Timestamp::new_test_instance());
        assert!(!queue.blocked_half_full());

        queue.priority_set(&account2, Priority::INITIAL);
        queue.block(account2, BlockHash::from(2), Timestamp::new_test_instance());
        assert!(queue.blocked_half_full());
    }

    #[test]
    fn container_info() {
        let mut queue = BootstrapQueue::default();
        queue.priority_set(&Account::from(1), Priority::INITIAL);
        queue.priority_set(&Account::from(2), Priority::INITIAL);
        queue.priority_set(&Account::from(3), Priority::INITIAL);
        queue.block(
            Account::from(2),
            BlockHash::from(3),
            Timestamp::new_test_instance(),
        );
        queue.dependency_update(&BlockHash::from(3), Account::from(1000));
        queue.block(
            Account::from(3),
            BlockHash::from(4),
            Timestamp::new_test_instance(),
        );
        let info = queue.container_info();
        assert_eq!(
            info,
            [
                ("priorities", 2, 0),
                ("blocked", 2, 0),
                ("blocked_unknown", 1, 0)
            ]
            .into()
        )
    }
}
