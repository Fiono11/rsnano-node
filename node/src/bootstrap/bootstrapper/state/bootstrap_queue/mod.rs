use std::{cmp::min, time::Duration};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, BlockHash};
use rsnano_utils::container_info::ContainerInfo;

mod blocked;
mod prioritized;
mod priority;

use blocked::BlockedAccounts;
pub use blocked::BlockedBlock;
use prioritized::{ChangePriorityResult, PrioritizedAccount, PrioritizedAccounts};
pub use priority::Priority;

#[derive(Clone, Debug, PartialEq)]
pub struct BootstrapQueueConfig {
    pub max_prioritized_accounts: usize,
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
            max_prioritized_accounts: 256 * 1024,
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
    priorities: PrioritizedAccounts,
    blocked: BlockedAccounts,
    revision: u64,
}

impl BootstrapQueue {
    pub const PRIORITY_INITIAL: Priority = Priority::new(2.0);
    pub const PRIORITY_INCREASE: Priority = Priority::new(2.0);
    pub const PRIORITY_DIVIDE: f64 = 2.0;
    pub const PRIORITY_MAX: Priority = Priority::new(128.0);
    pub const PRIORITY_CUTOFF: Priority = Priority::new(0.15);
    pub const MAX_FAILS: usize = 3;

    pub fn new(config: BootstrapQueueConfig) -> Self {
        Self {
            config,
            priorities: Default::default(),
            blocked: Default::default(),
            revision: 0,
        }
    }

    pub fn priority_up(&mut self, account: &Account) -> PriorityUpResult {
        if account.is_zero() {
            return PriorityUpResult::AccountBlocked;
        }

        if !self.blocked(account) {
            let updated = self.priorities.modify(account, |entry| {
                entry.priority = Self::higher_priority(entry.priority);
                entry.fails = 0;
                true // keep this entry
            });
            self.revision += 1;

            match updated {
                ChangePriorityResult::Updated | ChangePriorityResult::Deleted => {
                    PriorityUpResult::Updated
                }
                ChangePriorityResult::NotFound => {
                    self.priorities
                        .insert(PrioritizedAccount::new(*account, Self::PRIORITY_INITIAL));

                    self.trim_overflow();
                    PriorityUpResult::Inserted
                }
            }
        } else {
            PriorityUpResult::AccountBlocked
        }
    }

    fn higher_priority(priority: Priority) -> Priority {
        min(priority + Self::PRIORITY_INCREASE, Self::PRIORITY_MAX)
    }

    pub fn priority_down(&mut self, account: &Account) -> PriorityDownResult {
        if account.is_zero() {
            return PriorityDownResult::InvalidAccount;
        }

        let change_result = self.priorities.modify(account, |entry| {
            let new_priority = entry.priority / Self::PRIORITY_DIVIDE;
            if entry.fails >= BootstrapQueue::MAX_FAILS
                || entry.fails as f64 >= entry.priority.as_f64()
                || new_priority <= Self::PRIORITY_CUTOFF
            {
                false // delete entry
            } else {
                entry.fails += 1;
                entry.priority = new_priority;
                true // keep
            }
        });

        self.revision += 1;
        match change_result {
            ChangePriorityResult::Updated => PriorityDownResult::Deprioritized,
            ChangePriorityResult::Deleted => PriorityDownResult::Erased,
            ChangePriorityResult::NotFound => PriorityDownResult::AccountNotFound,
        }
    }

    pub fn priority_set(&mut self, account: &Account, priority: Priority) -> bool {
        let inserted =
            Self::priority_set_impl(account, priority, &self.blocked, &mut self.priorities);
        self.trim_overflow();
        self.revision += 1;
        inserted
    }

    fn priority_set_impl(
        account: &Account,
        priority: Priority,
        blocked: &BlockedAccounts,
        priorities: &mut PrioritizedAccounts,
    ) -> bool {
        if account.is_zero() || blocked.contains(account) || priorities.contains(account) {
            false
        } else {
            priorities.insert(PrioritizedAccount::new(*account, priority));
            true
        }
    }

    pub fn priority_erase(&mut self, account: &Account) -> bool {
        if account.is_zero() {
            return false;
        }

        self.revision += 1;
        self.priorities.remove(account).is_some()
    }

    pub fn block(&mut self, account: Account, dependency: BlockHash, now: Timestamp) -> bool {
        debug_assert!(!account.is_zero());

        let removed = self.priorities.remove(&account);

        if removed.is_some() {
            self.blocked.insert(BlockedBlock {
                account,
                dependency_block: dependency,
                dependency_account: Account::ZERO,
                added: now,
            });

            self.trim_overflow();
            self.revision += 1;
            true
        } else {
            false
        }
    }

    pub fn unblock(&mut self, account: Account, hash: Option<BlockHash>) -> bool {
        if account.is_zero() {
            return false;
        }

        // Unblock only if the dependency is fulfilled
        if let Some(existing) = self.blocked.get(&account) {
            let hash_matches = if let Some(hash) = hash {
                hash == existing.dependency_block
            } else {
                true
            };

            if hash_matches {
                debug_assert!(!self.priorities.contains(&account));
                self.priorities
                    .insert(PrioritizedAccount::new(account, Self::PRIORITY_INITIAL));
                self.blocked.remove_account(&account);
                self.trim_overflow();
                self.revision += 1;
                return true;
            }
        }

        false
    }

    /// Should be called periodically to remove old entries from the blocked accounts
    pub fn decay_blocked_accounts(&mut self, now: Timestamp) -> usize {
        let cutoff = now - self.config.blocked_decay;
        self.revision += 1;
        self.blocked.remove_older_than(cutoff)
    }

    #[allow(dead_code)]
    pub fn last_request(&self, account: &Account) -> Option<Timestamp> {
        self.priorities.get(account).and_then(|i| i.last_request)
    }

    pub fn set_last_request(&mut self, account: &Account, now: Timestamp) {
        debug_assert!(!account.is_zero());
        self.priorities.set_last_request(account, Some(now));
        self.revision += 1;
    }

    pub fn reset_last_request(&mut self, account: &Account) {
        debug_assert!(!account.is_zero());

        self.priorities.set_last_request(account, None);
        self.revision += 1;
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

        if updated > 0
            && !self.queue_full()
            && Self::priority_set_impl(
                &dependency_account,
                Self::PRIORITY_INITIAL,
                &self.blocked,
                &mut self.priorities,
            )
        {
            self.trim_overflow();
            self.revision += 1;
        }

        updated
    }

    /// Erase the oldest entries
    fn trim_overflow(&mut self) {
        while !self.priorities.is_empty()
            && self.priorities.len() > self.config.max_prioritized_accounts
        {
            self.priorities.pop_lowest_prio();
        }
        while self.blocked.len() > self.config.max_blocked_accounts {
            self.blocked.remove_oldest();
        }
    }

    pub fn next_target(
        &self,
        now: Timestamp,
        filter: impl Fn(&Account) -> bool,
    ) -> BootstrapTarget {
        if self.priorities.is_empty() {
            return Default::default();
        }

        let cutoff = now - self.config.account_cooldown;

        let Some(entry) = self.priorities.next_priority(cutoff, filter) else {
            return Default::default();
        };

        BootstrapTarget {
            account: entry.account,
            priority: entry.priority,
            fails: entry.fails,
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
        let mut inserted = 0;

        // Sample all accounts with a known dependency account (> account 0)
        let begin = Account::from(1);
        for entry in self.blocked.iter_start_dep_account(begin) {
            if self.queue_full() {
                break;
            }

            if Self::priority_set_impl(
                &entry.dependency_account,
                Self::PRIORITY_INITIAL,
                &self.blocked,
                &mut self.priorities,
            ) {
                inserted += 1;
            }
        }

        self.trim_overflow();
        self.revision += 1;
        inserted
    }

    pub fn blocked(&self, account: &Account) -> bool {
        self.blocked.contains(account)
    }

    pub fn prioritized(&self, account: &Account) -> bool {
        self.priorities.contains(account)
    }

    pub fn prioritized_len(&self) -> usize {
        self.priorities.len()
    }

    pub fn blocked_len(&self) -> usize {
        self.blocked.len()
    }

    pub fn unique_blocked_accounts(&self) -> usize {
        self.blocked.unique_dependency_accounts()
    }

    pub fn known_dependencies(&self) -> usize {
        self.blocked.known_dependencies()
    }

    pub fn iter_priorities(&self) -> impl Iterator<Item = (Priority, &Account)> {
        self.priorities.iter()
    }

    pub fn iter_blocked(&self) -> impl Iterator<Item = &BlockedBlock> {
        self.blocked.iter_by_insertion_order()
    }

    pub fn queue_full(&self) -> bool {
        self.priorities.len() >= self.config.max_prioritized_accounts
    }

    pub fn queue_half_full(&self) -> bool {
        self.priorities.len() > self.config.max_prioritized_accounts / 2
    }

    pub fn blocked_half_full(&self) -> bool {
        self.blocked.len() > self.config.max_blocked_accounts / 2
    }

    /// Accounts in the ledger but not in priority list are assumed priority 1.0f
    /// Blocked accounts are assumed priority 0.0f
    #[allow(dead_code)]
    pub fn priority(&self, account: &Account) -> Priority {
        if !self.blocked(account)
            && let Some(existing) = self.priorities.get(account)
        {
            return existing.priority;
        }
        Priority::ZERO
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.priorities.clear();
        self.blocked.clear();
        self.revision += 1;
    }

    pub fn clear_blocked_accounts(&mut self) {
        self.blocked.clear();
        self.revision += 1;
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn container_info(&self) -> ContainerInfo {
        // Count blocked entries with their dependency account unknown
        let blocked_unknown = self.blocked.count_by_dependency_account(&Account::ZERO);
        [
            (
                "priorities",
                self.priorities.len(),
                PrioritizedAccounts::ELEMENT_SIZE,
            ),
            ("blocked", self.blocked.len(), BlockedAccounts::ELEMENT_SIZE),
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
        assert_eq!(queue.blocked_len(), 0);
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
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
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
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
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
        assert_eq!(queue.priority(&account), BootstrapQueue::PRIORITY_INITIAL);

        queue.block(account, hash, Timestamp::new_test_instance());
        queue.unblock(account, None);

        assert_eq!(queue.priority(&account), BootstrapQueue::PRIORITY_INITIAL);
    }

    #[test]
    fn priority_up_down() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);

        queue.priority_up(&account);
        assert_eq!(queue.priority(&account), BootstrapQueue::PRIORITY_INITIAL);

        queue.priority_down(&account);
        assert_eq!(
            queue.priority(&account),
            BootstrapQueue::PRIORITY_INITIAL / BootstrapQueue::PRIORITY_DIVIDE
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
        assert_eq!(queue.priority(&account), BootstrapQueue::PRIORITY_MAX);
    }

    #[test]
    fn priority_down_saturate() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_up(&account);
        assert_eq!(queue.priority(&account), BootstrapQueue::PRIORITY_INITIAL);
        for _ in 0..10 {
            queue.priority_down(&account);
        }
        assert_eq!(queue.prioritized(&account), false);
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
        assert_eq!(queue.blocked_len(), 0);
        assert_eq!(queue.prioritized_len(), 0);
    }

    #[test]
    fn priority_up_for_blocked_account_fails() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
        queue.block(account, BlockHash::from(2), Timestamp::new_test_instance());
        let result = queue.priority_up(&account);
        assert_eq!(result, PriorityUpResult::AccountBlocked);
        assert_eq!(queue.blocked_len(), 1);
    }

    #[test]
    fn priority_down_for_zero_account_fails() {
        let mut queue = BootstrapQueue::default();
        let result = queue.priority_down(&Account::ZERO);
        assert_eq!(result, PriorityDownResult::InvalidAccount);
        assert_eq!(queue.blocked_len(), 0);
        assert_eq!(queue.prioritized_len(), 0);
    }

    #[test]
    fn priority_set_for_zero_account_fails() {
        let mut queue = BootstrapQueue::default();
        let success = queue.priority_set(&Account::ZERO, BootstrapQueue::PRIORITY_INITIAL);
        assert_eq!(success, false);
        assert_eq!(queue.blocked_len(), 0);
        assert_eq!(queue.prioritized_len(), 0);
    }

    #[test]
    fn priority_set_fails_for_blocked_account() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
        queue.block(account, BlockHash::from(2), Timestamp::new_test_instance());
        let success = queue.priority_set(&account, Priority::new(42.0));

        assert_eq!(success, false);
        assert_eq!(queue.blocked_len(), 1);
        assert_eq!(queue.prioritized_len(), 0);
    }

    #[test]
    fn priority_erase() {
        let mut queue = BootstrapQueue::default();
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        queue.priority_set(&account1, BootstrapQueue::PRIORITY_INITIAL);
        queue.priority_set(&account2, BootstrapQueue::PRIORITY_INITIAL);
        let removed = queue.priority_erase(&account1);
        assert!(removed);
        assert!(!queue.prioritized(&account1));
        assert!(queue.prioritized(&account2));
    }

    #[test]
    fn priority_erase_zero_account() {
        let mut queue = BootstrapQueue::default();
        assert!(!queue.priority_erase(&Account::ZERO));
    }

    #[test]
    fn set_last_request_for_unknown_account_does_nothing() {
        let mut queue = BootstrapQueue::default();
        queue.set_last_request(&Account::from(1), Timestamp::new_test_instance());
        assert_eq!(queue.prioritized_len(), 0);
    }

    #[test]
    fn set_last_request() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
        let new_timestamp = Timestamp::new_test_instance() + Duration::from_secs(1000);
        queue.set_last_request(&account, new_timestamp);
        assert_eq!(queue.last_request(&account), Some(new_timestamp))
    }

    #[test]
    fn timestamp_reset() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
        queue.reset_last_request(&account);
        assert_eq!(queue.priorities.get(&account).unwrap().last_request, None);
    }

    #[test]
    fn trim_priorities_on_overflow() {
        let mut queue = BootstrapQueue::new(BootstrapQueueConfig {
            max_prioritized_accounts: 2,
            ..Default::default()
        });
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(3);
        queue.priority_set(&account1, Priority::new(2.0));
        queue.priority_set(&account2, Priority::new(1.0));
        queue.priority_set(&account3, Priority::new(3.0));

        assert_eq!(queue.prioritized_len(), 2);
        assert!(queue.prioritized(&account1));
        assert!(queue.prioritized(&account3));
        assert!(!queue.prioritized(&account2));
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

        assert_eq!(queue.blocked_len(), 2);
        assert!(queue.blocked(&account2));
        assert!(queue.blocked(&account3));
        assert!(!queue.blocked(&account1));
    }

    #[test]
    fn next_priority_empty() {
        let queue = BootstrapQueue::default();
        let next = queue.next_target(Timestamp::new_test_instance(), |_| true);
        assert_eq!(next, BootstrapTarget::default());
    }

    #[test]
    fn next_priority() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
        let now = Timestamp::new_test_instance();
        let next = queue.next_target(now, |_| true);
        assert_eq!(
            next,
            BootstrapTarget {
                account,
                priority: BootstrapQueue::PRIORITY_INITIAL,
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
        let next = queue.next_target(now, |_| true);
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
        let next = queue.next_target(now, |_| true);
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
        queue.priority_set(&account1, BootstrapQueue::PRIORITY_INITIAL);
        queue.priority_set(&account2, BootstrapQueue::PRIORITY_INITIAL);
        queue.priority_set(&account3, BootstrapQueue::PRIORITY_INITIAL);
        let now = Timestamp::new_test_instance();
        let next = queue.next_target(now, |a| *a == account2);
        assert_eq!(
            next,
            BootstrapTarget {
                account: account2,
                priority: BootstrapQueue::PRIORITY_INITIAL,
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
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
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
        queue.priority_set(&account1, BootstrapQueue::PRIORITY_INITIAL);
        queue.priority_set(&account2, BootstrapQueue::PRIORITY_INITIAL);
        queue.priority_set(&account3, BootstrapQueue::PRIORITY_INITIAL);
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
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());

        queue.dependency_update(&dependency, dependency_account);

        assert!(queue.prioritized(&dependency_account));
    }

    #[test]
    fn sync_dependencies_doesnt_insert_when_dependency_account_already_prioritized() {
        let mut queue = BootstrapQueue::default();
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());
        queue.dependency_update(&dependency, dependency_account);
        queue.priority_set(&dependency_account, BootstrapQueue::PRIORITY_INITIAL);

        let inserted = queue.sync_dependencies();

        assert_eq!(inserted, 0);
    }

    #[test]
    fn sync_dependencies_doesnt_insert_when_max_accounts_prioritized() {
        let config = BootstrapQueueConfig {
            max_prioritized_accounts: 2,
            ..Default::default()
        };
        let mut queue = BootstrapQueue::new(config);
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        queue.priority_set(&account, BootstrapQueue::PRIORITY_INITIAL);
        queue.block(account, dependency, Timestamp::new_test_instance());
        queue.dependency_update(&dependency, dependency_account);
        queue.priority_set(&Account::from(9999), BootstrapQueue::PRIORITY_INITIAL);
        queue.priority_set(&Account::from(8888), BootstrapQueue::PRIORITY_INITIAL);

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

        queue.priority_set(&account1, BootstrapQueue::PRIORITY_INITIAL);
        queue.block(account1, BlockHash::from(1), Timestamp::new_test_instance());
        assert!(!queue.blocked_half_full());

        queue.priority_set(&account2, BootstrapQueue::PRIORITY_INITIAL);
        queue.block(account2, BlockHash::from(2), Timestamp::new_test_instance());
        assert!(queue.blocked_half_full());
    }

    #[test]
    fn container_info() {
        let mut queue = BootstrapQueue::default();
        queue.priority_set(&Account::from(1), BootstrapQueue::PRIORITY_INITIAL);
        queue.priority_set(&Account::from(2), BootstrapQueue::PRIORITY_INITIAL);
        queue.priority_set(&Account::from(3), BootstrapQueue::PRIORITY_INITIAL);
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
                ("priorities", 2, PrioritizedAccounts::ELEMENT_SIZE),
                ("blocked", 2, BlockedAccounts::ELEMENT_SIZE),
                ("blocked_unknown", 1, 0)
            ]
            .into()
        )
    }
}
