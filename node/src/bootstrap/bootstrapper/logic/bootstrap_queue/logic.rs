use std::{cmp::max, collections::VecDeque, time::Duration};

use rustc_hash::FxHashMap;

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, Block, BlockHash};
use rsnano_utils::container_info::ContainerInfo;

use super::{
    blocked::BlockedAccounts,
    bootstrapping_account::{AccountState, BlockedInfo, BootstrappingAccount},
    download_queue::{ChangePriorityResult, DownloadQueue},
    downloading::DownloadingAccounts,
    single_block_account_set::SingleBlockAccountSet,
};
use crate::bootstrap::bootstrapper::logic::Priority;

#[derive(Default)]
pub struct BootstrapQueueSnapshot {
    pub info: BootstrapQueueInfo,
    pub download_queue: Vec<BootstrappingAccountInfo>,
    pub downloading: Vec<BootstrappingAccountInfo>,
    pub blocked: Vec<BootstrappingAccountInfo>,
}

#[derive(Default)]
pub struct BootstrapQueueInfo {
    pub download_queue: usize,
    pub unblocked: usize,
    pub downloading: usize,
    pub ready_to_process: usize,
    pub processing: usize,
    pub blocked: usize,
    pub unknown_dependencies: usize,
    pub unique_blocking_accounts: usize,
    pub cached_blocks: usize,
    pub discarded_blocks: usize,
}

pub struct BootstrappingAccountInfo {
    pub account: Account,
    pub priority: Priority,
    pub dependency_block: BlockHash,
    pub dependency_account: Account,
}

impl From<&BootstrappingAccount> for BootstrappingAccountInfo {
    fn from(e: &BootstrappingAccount) -> Self {
        let (dependency_block, dependency_account) = e
            .blocked
            .as_ref()
            .map(|b| (b.dependency_block, b.dependency_account.unwrap_or_default()))
            .unwrap_or_default();
        Self {
            account: e.account,
            priority: e.priority,
            dependency_block,
            dependency_account,
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
    Upgraded,
    InvalidAccount,
    /// If the account is not in the download queue, we don't change its priority
    Unchanged,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PriorityDownResult {
    Deprioritized,
    /// The priority got too low, so the account got erased
    Removed,
    AccountNotFound,
}

/// A prioritized queue of accounts which should bootstrapped.
/// Accounts can be blocked, because a dependency block is missing. Blocked accounts
/// are put on hold.
pub(crate) struct BootstrapQueueLogic {
    config: BootstrapQueueConfig,
    accounts: FxHashMap<Account, BootstrappingAccount>,
    download_queue: DownloadQueue,
    downloading: DownloadingAccounts,
    ready_to_process: SingleBlockAccountSet,
    processing: SingleBlockAccountSet,
    blocked: BlockedAccounts,
    revision: u64,
    cached_blocks: usize,
    discarded_blocks: usize,
}

impl BootstrapQueueLogic {
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
            cached_blocks: 0,
            discarded_blocks: 0,
        }
    }

    pub fn priority_up_to(
        &mut self,
        account: &Account,
        new_priority: Priority,
    ) -> PriorityUpResult {
        if account.is_zero() {
            return PriorityUpResult::InvalidAccount;
        }

        let result = self.modify_priority(account, |e| {
            if e.priority < new_priority {
                e.priority = new_priority;
            }
            e.fails = 0;
        });
        self.revision += 1;

        match result {
            ChangePriorityResult::Updated => PriorityUpResult::Upgraded,
            ChangePriorityResult::Removed => unreachable!(),
            ChangePriorityResult::Unchanged => PriorityUpResult::Unchanged,
            ChangePriorityResult::NotFound => {
                self.accounts.insert(
                    *account,
                    BootstrappingAccount::new(*account, max(new_priority, Priority::CUTOFF)),
                );
                self.download_queue.insert(*account, new_priority);
                self.trim_overflow();
                PriorityUpResult::Inserted
            }
        }
    }

    pub fn priority_up(&mut self, account: &Account) -> PriorityUpResult {
        if account.is_zero() {
            return PriorityUpResult::InvalidAccount;
        }

        let result = self.modify_priority(account, |e| {
            e.priority = e.priority.increase();
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
            PriorityUpResult::Upgraded
        }
    }

    pub fn priority_down(&mut self, account: &Account) -> PriorityDownResult {
        let change_result = self.modify_priority(account, |e| {
            e.priority = e.priority / Priority::DIVIDE;
            e.fails += 1;
        });

        self.revision += 1;
        match change_result {
            ChangePriorityResult::Updated => PriorityDownResult::Deprioritized,
            ChangePriorityResult::Removed => PriorityDownResult::Removed,
            ChangePriorityResult::NotFound => PriorityDownResult::AccountNotFound,
            ChangePriorityResult::Unchanged => {
                unreachable!("the account is ether downgraded, removed or not found")
            }
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
        if entry.priority == old_prio {
            return ChangePriorityResult::Unchanged;
        }

        if entry.state == AccountState::EnqueuedForDownload {
            self.download_queue
                .change_priority(account, old_prio, entry.priority);
        }

        if entry.fails >= BootstrapQueueLogic::MAX_FAILS
            || entry.fails as f64 > entry.priority.as_f64()
            || entry.priority < Priority::CUTOFF
        {
            self.remove(account);
            return ChangePriorityResult::Removed;
        }

        ChangePriorityResult::Updated
    }

    pub fn remove(&mut self, account: &Account) -> bool {
        let mut to_remove = VecDeque::new();
        to_remove.push_back(*account);
        while let Some(account) = to_remove.pop_front() {
            let Some(removed) = self.accounts.remove(&account) else {
                continue;
            };
            self.cached_blocks -= removed.blocks.len();
            self.discarded_blocks += removed.blocks.len();
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

    pub fn unblock(&mut self, account: Account) -> bool {
        let Some(entry) = self.accounts.get_mut(&account) else {
            return false;
        };
        if entry.state != AccountState::Blocked {
            return false;
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
        for account in &removed {
            self.accounts.remove(account);
        }
        removed.len()
    }

    #[cfg(test)]
    pub fn last_request(&self, account: &Account) -> Option<Timestamp> {
        self.accounts.get(account)?.last_request
    }

    pub fn set_last_request(&mut self, account: &Account, now: Timestamp) {
        if let Some(entry) = self.accounts.get_mut(account) {
            entry.last_request = Some(now);
            self.revision += 1;
        }
    }

    /// Sets information about the account chain that contains the block hash
    pub fn dependency_update(
        &mut self,
        dependency: &BlockHash,
        dependency_account: Account,
    ) -> usize {
        let updated = self
            .blocked
            .modify_dependency_account(dependency, dependency_account);

        for account in &updated {
            let entry = self.accounts.get_mut(account).unwrap();
            entry.blocked.as_mut().unwrap().dependency_account = Some(dependency_account);
        }

        if !updated.is_empty() && !self.queue_full() {
            self.priority_up(&dependency_account);
        }

        updated.len()
    }

    /// Erase the oldest entries
    fn trim_overflow(&mut self) {
        while self.needs_trimming() {
            let account = self.download_queue.pop_lowest_prio().unwrap();
            self.accounts.remove(&account);
        }

        while self.blocked.len() > self.config.max_blocked_accounts {
            let to_remove = *self.blocked.oldest().unwrap();
            self.remove(&to_remove);
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
                filter(account)
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

    pub fn download_started(&mut self, account: &Account, now: Timestamp) -> bool {
        let Some(entry) = self.accounts.get_mut(&account) else {
            return false;
        };
        if entry.state != AccountState::EnqueuedForDownload {
            return false;
        }
        self.download_queue.remove(account, entry.priority);
        entry.state = AccountState::Downloading;
        self.downloading.insert(*account, now);
        true
    }

    pub fn download_finished(&mut self, account: &Account, blocks: VecDeque<Block>) -> bool {
        let Some(entry) = self.accounts.get_mut(account) else {
            return false;
        };
        if entry.state != AccountState::Downloading {
            return false;
        }
        self.downloading.remove(account);
        self.cached_blocks += blocks.len();
        debug_assert!(entry.blocks.is_empty());
        entry.blocks = blocks;
        entry.last_request = None;
        if entry.blocks.is_empty() {
            entry.state = AccountState::EnqueuedForDownload;
            self.download_queue.insert(*account, entry.priority);
        } else {
            entry.state = AccountState::ReadyToProcess;
            self.ready_to_process
                .insert(*account, entry.first_block_hash().unwrap());
        }
        true
    }

    pub fn processing_started(&mut self, block_hash: &BlockHash) -> bool {
        let Some(account) = self.ready_to_process.remove_block(block_hash) else {
            return false;
        };
        let entry = self.accounts.get_mut(&account).unwrap();
        entry.state = AccountState::Processing;
        self.processing.insert(account, *block_hash);
        true
    }

    pub fn reprocess(&mut self, account: &Account, block_hash: &BlockHash) -> bool {
        if self.processing.remove_block(block_hash).is_some() {
            let entry = self.accounts.get_mut(&account).unwrap();
            entry.state = AccountState::ReadyToProcess;
            self.ready_to_process.insert(*account, *block_hash);
            true
        } else {
            false
        }
    }

    pub fn processing_finished(&mut self, block_hash: &BlockHash) -> Option<AccountState> {
        if let Some(account) = self.processing.remove_block(block_hash) {
            self.cached_blocks -= 1;
            let entry = self.accounts.get_mut(&account).unwrap();
            let first_block = entry.blocks.pop_front().unwrap();
            assert_eq!(first_block.hash(), *block_hash);
            if entry.blocks.is_empty() {
                entry.state = AccountState::EnqueuedForDownload;
                self.download_queue.insert(account, entry.priority);
                Some(entry.state)
            } else {
                entry.state = AccountState::ReadyToProcess;
                self.ready_to_process
                    .insert(account, entry.first_block_hash().unwrap());
                Some(entry.state)
            }
        } else {
            None
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

        let mut accounts_to_enqueue = Vec::new();
        // Sample all accounts with a known dependency account (> account 0)
        let begin = Account::from(1);
        for blocked_account in self.blocked.iter_start_dep_account(begin) {
            if self.queue_full() {
                break;
            }

            let entry = self.accounts.get(blocked_account).unwrap();
            let dep_account = entry
                .blocked
                .as_ref()
                .unwrap()
                .dependency_account
                .expect("should be set");

            if !self.contains(&dep_account) {
                accounts_to_enqueue.push(dep_account);
            }
        }

        for account in accounts_to_enqueue {
            if self.queue_full() {
                break;
            }

            self.priority_up_to(&account, Priority::INITIAL);
            inserted += 1;
        }

        inserted
    }

    #[cfg(test)]
    pub fn blocked(&self, account: &Account) -> bool {
        self.blocked.contains(account)
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.accounts.contains_key(account)
    }

    pub fn unblocked_count(&self) -> usize {
        self.accounts.len() - self.blocked.len()
    }

    pub fn snapshot(&self, limit: usize, filter: Option<Account>) -> BootstrapQueueSnapshot {
        let download_queue = self
            .iter_download_queue()
            .filter(|e| filter.is_none() || filter == Some(e.account))
            .take(limit)
            .map(|e| e.into())
            .collect();

        let downloading = self
            .iter_downloading()
            .filter(|e| filter.is_none() || filter == Some(e.account))
            .take(limit)
            .map(|e| e.into())
            .collect();

        let blocked = self
            .iter_blocked()
            .filter(|e| {
                filter.is_none()
                    || filter == Some(e.account)
                    || filter == e.blocked.as_ref().and_then(|b| b.dependency_account)
            })
            .take(limit)
            .map(|e| e.into())
            .collect();

        BootstrapQueueSnapshot {
            info: self.info(),
            download_queue,
            downloading,
            blocked,
        }
    }

    pub fn info(&self) -> BootstrapQueueInfo {
        BootstrapQueueInfo {
            download_queue: self.download_queue.len(),
            unblocked: self.unblocked_count(),
            downloading: self.downloading.len(),
            ready_to_process: self.ready_to_process.len(),
            processing: self.processing.len(),
            blocked: self.blocked.len(),
            unknown_dependencies: self.blocked.len() - self.blocked.known_dependencies(),
            unique_blocking_accounts: self.blocked.unique_dependency_accounts(),
            cached_blocks: self.cached_blocks,
            discarded_blocks: self.discarded_blocks,
        }
    }

    fn iter_download_queue(&self) -> impl Iterator<Item = &BootstrappingAccount> {
        self.download_queue
            .iter()
            .map(|(_, account)| self.accounts.get(account).unwrap())
    }

    fn iter_downloading(&self) -> impl Iterator<Item = &BootstrappingAccount> {
        self.accounts
            .values()
            .filter(|a| a.state == AccountState::Downloading)
    }

    fn iter_blocked(&self) -> impl Iterator<Item = &BootstrappingAccount> {
        self.blocked
            .iter_by_insertion_order()
            .map(|account| self.accounts.get(account).unwrap())
    }

    fn queue_full(&self) -> bool {
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
    #[cfg(test)]
    pub fn priority(&self, account: &Account) -> Priority {
        self.accounts
            .get(account)
            .map(|a| a.priority)
            .unwrap_or(Priority::ZERO)
    }

    pub fn clear_blocked_accounts(&mut self) {
        let to_remove: Vec<_> = self
            .accounts
            .values()
            .filter_map(|a| {
                if a.state == AccountState::Blocked {
                    Some(a.account)
                } else {
                    None
                }
            })
            .collect();
        for account in to_remove {
            self.remove(&account);
        }
        self.revision += 1;
    }

    pub fn timeout(&mut self, now: Timestamp) {
        while let Some(account) = self.downloading.pop_timeout(now) {
            let entry = self.accounts.get_mut(&account).unwrap();
            entry.state = AccountState::EnqueuedForDownload;
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
            ("download_queue", self.download_queue.len(), 0),
            ("blocked", self.blocked.len(), 0),
            ("blocked_unknown", blocked_unknown, 0),
            ("unblocked", self.unblocked_count(), 0),
            ("downloading", self.downloading.len(), 0),
            ("ready_to_process", self.ready_to_process.len(), 0),
            ("processing", self.processing.len(), 0),
            ("cached_blocks", self.cached_blocks, 0),
        ]
        .into()
    }
}

impl Default for BootstrapQueueLogic {
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
    fn new_queue_is_empty() {
        let queue = BootstrapQueueLogic::default();
        assert!(!queue.contains(&Account::from(1)));
        assert!(!queue.blocked(&Account::from(1)));
    }
    /*
     * Setting priority
     */

    #[test]
    fn priority_can_be_set() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        let prio = Priority::new(10.0);

        queue.priority_up_to(&account, prio);

        assert_eq!(queue.priority(&account), prio);
    }

    #[test]
    fn priority_set_fails_for_blocked_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.priority_up_to(&account, Priority::INITIAL);
        queue.block(account, BlockHash::from(2), Timestamp::new_test_instance());
        queue.priority_up_to(&account, Priority::new(42.0));

        assert_eq!(queue.info().blocked, 1);
        assert_eq!(queue.info().download_queue, 0);
    }

    #[test]
    fn account_that_isnt_in_the_queue_has_no_priority() {
        let queue = BootstrapQueueLogic::default();
        assert_eq!(queue.priority(&Account::from(1)), Priority::ZERO);
    }

    #[test]
    fn priority_up_cant_reduce_the_priority() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.priority_up_to(&account, Priority::new(2.0));
        queue.priority_up_to(&account, Priority::new(1.0));
        assert_eq!(queue.priority(&account), Priority::new(2.0));
    }

    /*
     * Increasing priority
     */

    #[test]
    fn priority_has_an_upper_limit() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);

        for _ in 0..100 {
            queue.priority_up(&account);
        }

        assert_eq!(queue.priority(&account), Priority::MAX);
    }

    #[test]
    fn zero_account_cant_be_prioritized() {
        let mut queue = BootstrapQueueLogic::default();
        assert_eq!(
            queue.priority_up(&Account::ZERO),
            PriorityUpResult::InvalidAccount
        );
        assert_eq!(
            queue.priority_up_to(&Account::ZERO, Priority::INITIAL),
            PriorityUpResult::InvalidAccount
        );
        assert_eq!(queue.info().blocked, 0);
        assert_eq!(queue.info().download_queue, 0);
    }

    #[test]
    fn priority_can_be_increased_for_blocked_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.priority_up_to(&account, Priority::INITIAL);
        queue.block(account, BlockHash::from(2), Timestamp::new_test_instance());

        let result = queue.priority_up(&account);

        assert_eq!(result, PriorityUpResult::Upgraded);
        assert_eq!(queue.info().blocked, 1);
        assert_eq!(
            queue.priority(&account),
            Priority::INITIAL + Priority::INCREASE
        );
    }

    /*
     * Decreasing priority
     */

    #[test]
    fn priority_down_decreases_priority() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.priority_up_to(&account, Priority::INITIAL);

        queue.priority_down(&account);

        assert_eq!(
            queue.priority(&account),
            Priority::INITIAL / Priority::DIVIDE
        );
    }

    #[test]
    fn priority_down_does_nothing_if_account_not_enqueued() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);

        queue.priority_down(&account);

        assert_eq!(queue.priority(&account), Priority::ZERO);
    }

    #[test]
    fn account_gets_dequeued_if_priority_gets_too_low() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.priority_up_to(&account, Priority::INITIAL);

        for _ in 0..10 {
            queue.priority_down(&account);
        }

        assert!(!queue.contains(&account));
    }

    /*
     * Blocking an account
     */

    #[test]
    fn block_blocks_an_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);
        queue.priority_up_to(&account, Priority::INITIAL);

        queue.block(account, hash, Timestamp::new_test_instance());

        assert!(queue.blocked(&account));
        assert_eq!(queue.info().download_queue, 0);
        assert_eq!(queue.info().blocked, 1);
    }

    #[test]
    fn blocking_unknown_account_does_nothing() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        let blocked = queue.block(account, BlockHash::from(2), Timestamp::new_test_instance());
        assert!(!blocked);
        assert_eq!(queue.info().blocked, 0);
        assert!(!queue.blocked(&account));
    }

    /*
     * Unblocking an account
     */

    #[test]
    fn unblock_unblocks_the_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);
        queue.priority_up(&account);
        queue.block(account, hash, Timestamp::new_test_instance());

        assert!(queue.unblock(account));

        assert_eq!(queue.blocked(&account), false);
        assert_eq!(queue.info().download_queue, 1);
    }

    #[test]
    fn unblock_unknown_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        assert!(!queue.unblock(account));
        assert!(!queue.contains(&account));
    }

    #[test]
    fn priority_stays_unchanged_after_unblock() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);
        let priority = Priority::new(99.0);
        queue.priority_up_to(&account, priority);
        queue.block(account, hash, Timestamp::new_test_instance());

        queue.unblock(account);

        assert_eq!(queue.priority(&account), priority);
    }

    /*
     * Misc
     */

    #[test]
    fn remove_removes_the_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        queue.priority_up_to(&account1, Priority::INITIAL);
        queue.priority_up_to(&account2, Priority::INITIAL);
        let removed = queue.remove(&account1);
        assert!(removed);
        assert!(!queue.contains(&account1));
        assert!(queue.contains(&account2));
    }

    #[test]
    fn set_last_request() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.priority_up_to(&account, Priority::INITIAL);
        let new_timestamp = Timestamp::new_test_instance() + Duration::from_secs(1000);
        queue.set_last_request(&account, new_timestamp);
        assert_eq!(queue.last_request(&account), Some(new_timestamp))
    }

    #[test]
    fn set_last_request_for_unknown_account_does_nothing() {
        let mut queue = BootstrapQueueLogic::default();
        queue.set_last_request(&Account::from(1), Timestamp::new_test_instance());
        assert_eq!(queue.info().download_queue, 0);
    }

    #[test]
    fn trim_priorities_on_overflow() {
        let mut queue = BootstrapQueueLogic::new(BootstrapQueueConfig {
            max_unblocked_accounts: 2,
            ..Default::default()
        });
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(3);
        queue.priority_up_to(&account1, Priority::new(2.0));
        queue.priority_up_to(&account2, Priority::new(1.0));
        queue.priority_up_to(&account3, Priority::new(3.0));

        assert_eq!(queue.info().download_queue, 2);
        assert!(queue.contains(&account1));
        assert!(queue.contains(&account3));
        assert!(!queue.contains(&account2));
    }

    #[test]
    fn trim_bocked_on_overflow() {
        let mut queue = BootstrapQueueLogic::new(BootstrapQueueConfig {
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

        assert_eq!(queue.info().blocked, 2);
        assert!(queue.blocked(&account2));
        assert!(queue.blocked(&account3));
        assert!(!queue.blocked(&account1));
    }

    #[test]
    fn next_priority_empty() {
        let queue = BootstrapQueueLogic::default();
        let next = queue.next_download_target(Timestamp::new_test_instance(), |_| true);
        assert_eq!(next, BootstrapTarget::default());
    }

    #[test]
    fn next_priority() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.priority_up_to(&account, Priority::INITIAL);
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
        let mut queue = BootstrapQueueLogic::default();
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
        let mut queue = BootstrapQueueLogic::new(config.clone());
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        queue.priority_up_to(&account1, Priority::new(100.0));
        queue.priority_up_to(&account2, Priority::new(1.0));
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
        let mut queue = BootstrapQueueLogic::new(config.clone());
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(2);
        queue.priority_up_to(&account1, Priority::INITIAL);
        queue.priority_up_to(&account2, Priority::INITIAL);
        queue.priority_up_to(&account3, Priority::INITIAL);
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
        let queue = BootstrapQueueLogic::default();
        assert_eq!(queue.next_blocked(|_| true), BlockHash::ZERO);
    }

    #[test]
    fn next_blocked() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        let dependency = BlockHash::from(2);
        queue.priority_up_to(&account, Priority::INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());
        assert_eq!(queue.next_blocked(|_| true), dependency);
    }

    #[test]
    fn next_blocked_filter() {
        let mut queue = BootstrapQueueLogic::default();
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(3);
        let dependency = BlockHash::from(2);
        queue.priority_up_to(&account1, Priority::INITIAL);
        queue.priority_up_to(&account2, Priority::INITIAL);
        queue.priority_up_to(&account3, Priority::INITIAL);
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
    fn blocked_half_full() {
        let config = BootstrapQueueConfig {
            max_blocked_accounts: 3,
            ..Default::default()
        };
        let mut queue = BootstrapQueueLogic::new(config);
        let account1 = Account::from(1);
        let account2 = Account::from(2);

        assert!(!queue.blocked_half_full());

        queue.priority_up_to(&account1, Priority::INITIAL);
        queue.block(account1, BlockHash::from(1), Timestamp::new_test_instance());
        assert!(!queue.blocked_half_full());

        queue.priority_up_to(&account2, Priority::INITIAL);
        queue.block(account2, BlockHash::from(2), Timestamp::new_test_instance());
        assert!(queue.blocked_half_full());
    }

    #[test]
    fn container_info() {
        let mut queue = BootstrapQueueLogic::default();
        queue.priority_up_to(&Account::from(1), Priority::INITIAL);
        queue.priority_up_to(&Account::from(2), Priority::INITIAL);
        queue.priority_up_to(&Account::from(3), Priority::INITIAL);
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
                ("download_queue", 2, 0),
                ("blocked", 2, 0),
                ("blocked_unknown", 1, 0),
                ("unblocked", 2, 0),
                ("downloading", 0, 0),
                ("ready_to_process", 0, 0),
                ("processing", 0, 0),
                ("cached_blocks", 0, 0),
            ]
            .into()
        )
    }

    /*
     * Sync sync_dependencies
     */

    #[test]
    fn sync_dependencies_empty() {
        let mut queue = BootstrapQueueLogic::default();
        let inserted = queue.sync_dependencies();
        assert_eq!(inserted, 0);
    }

    #[test]
    fn sync_dependencies_insert_one_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        queue.priority_up_to(&account, Priority::INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());

        queue.dependency_update(&dependency, dependency_account);

        assert!(queue.contains(&dependency_account));
    }

    #[test]
    fn sync_dependencies_doesnt_insert_when_dependency_account_already_prioritized() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        queue.priority_up_to(&account, Priority::INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());
        queue.dependency_update(&dependency, dependency_account);
        queue.priority_up_to(&dependency_account, Priority::INITIAL);

        let inserted = queue.sync_dependencies();

        assert_eq!(inserted, 0);
    }

    #[test]
    fn sync_dependencies_doesnt_insert_when_max_accounts_prioritized() {
        let config = BootstrapQueueConfig {
            max_unblocked_accounts: 2,
            ..Default::default()
        };
        let mut queue = BootstrapQueueLogic::new(config);
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        queue.priority_up_to(&account, Priority::INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());
        queue.dependency_update(&dependency, dependency_account);
        queue.priority_up_to(&Account::from(9999), Priority::INITIAL);
        queue.priority_up_to(&Account::from(8888), Priority::INITIAL);

        let inserted = queue.sync_dependencies();

        assert_eq!(inserted, 0);
    }

    /*
     * Snapshot
     */

    #[test]
    fn snapshot() {
        let mut queue = BootstrapQueueLogic::default();
        let queued = Account::from(1);
        let downloading = Account::from(2);
        let blocked = Account::from(3);
        let dependency = BlockHash::from(99);
        let now = Timestamp::new_test_instance();

        queue.priority_up_to(&queued, Priority::INITIAL);
        queue.priority_up_to(&downloading, Priority::INITIAL);
        queue.download_started(&downloading, now);
        queue.priority_up_to(&blocked, Priority::INITIAL);
        queue.block(blocked, dependency, now);

        let snap = queue.snapshot(10, None);

        assert_eq!(snap.info.download_queue, 1);
        assert_eq!(snap.download_queue.len(), 1);
        assert_eq!(snap.download_queue[0].account, queued);
        assert_eq!(snap.download_queue[0].priority, Priority::INITIAL);

        assert_eq!(snap.downloading.len(), 1);
        assert_eq!(snap.downloading[0].account, downloading);
        assert_eq!(snap.downloading[0].priority, Priority::INITIAL);

        assert_eq!(snap.blocked.len(), 1);
        assert_eq!(snap.blocked[0].account, blocked);
        assert_eq!(snap.blocked[0].dependency_block, dependency);
    }
}
