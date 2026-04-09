use std::{cmp::min, time::Duration};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, BlockHash};
use rsnano_utils::container_info::ContainerInfo;

mod blocked_accounts;
mod priority;
mod priority_container;

use blocked_accounts::{BlockedAccounts};
pub use blocked_accounts::BlockedAccount;
pub use priority::Priority;
use priority_container::{ChangePriorityResult, PriorityContainer, PriorityEntry};

#[derive(Clone, Debug, PartialEq)]
pub struct CandidateAccountsConfig {
    pub max_prioritized_accounts: usize,
    pub max_blocked_accounts: usize,

    /// A blocked account is removed if it has been blocked for blocked_decay
    pub blocked_decay: Duration,

    /// After a request was made for an account, the account goes into cooldown,
    /// so that we don't immediately create more requests for it, because we need
    /// to wait a bit for the responses to come in
    pub account_cooldown: Duration,
}

impl Default for CandidateAccountsConfig {
    fn default() -> Self {
        Self {
            max_prioritized_accounts: 256 * 1024,
            max_blocked_accounts: 256 * 1024,
            blocked_decay: Duration::from_secs(60 * 60),
            account_cooldown: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PriorityUpResult {
    Inserted,
    Updated,
    InvalidAccount,
    AccountBlocked,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PriorityDownResult {
    Deprioritized,
    Erased,
    AccountNotFound,
    InvalidAccount,
}

/// This struct tracks accounts which are candidates for the next bootstrap request or which are
/// blocked
pub struct CandidateAccounts {
    config: CandidateAccountsConfig,
    priorities: PriorityContainer,
    blocked: BlockedAccounts,
}

impl CandidateAccounts {
    pub const PRIORITY_INITIAL: Priority = Priority::new(1.0);
    pub const PRIORITY_INCREASE: Priority = Priority::new(2.0);
    pub const PRIORITY_DIVIDE: f64 = 2.0;
    pub const PRIORITY_MAX: Priority = Priority::new(128.0);
    pub const PRIORITY_CUTOFF: Priority = Priority::new(0.15);
    pub const MAX_FAILS: usize = 3;

    pub fn new(config: CandidateAccountsConfig) -> Self {
        Self {
            config,
            priorities: Default::default(),
            blocked: Default::default(),
        }
    }

    pub fn priority_up(&mut self, account: &Account) -> PriorityUpResult {
        if account.is_zero() {
            return PriorityUpResult::InvalidAccount;
        }

        if !self.blocked(account) {
            let updated = self.priorities.modify(account, |entry| {
                entry.priority = Self::higher_priority(entry.priority);
                entry.fails = 0;
                true // keep this entry
            });

            match updated {
                ChangePriorityResult::Updated | ChangePriorityResult::Deleted => {
                    PriorityUpResult::Updated
                }
                ChangePriorityResult::NotFound => {
                    self.priorities
                        .insert(PriorityEntry::new(*account, Self::PRIORITY_INITIAL));

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
            if entry.fails >= CandidateAccounts::MAX_FAILS
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

        match change_result {
            ChangePriorityResult::Updated => PriorityDownResult::Deprioritized,
            ChangePriorityResult::Deleted => PriorityDownResult::Erased,
            ChangePriorityResult::NotFound => PriorityDownResult::AccountNotFound,
        }
    }

    pub fn priority_set_initial(&mut self, account: &Account) -> bool {
        self.priority_set(account, Self::PRIORITY_INITIAL)
    }

    fn priority_set(&mut self, account: &Account, priority: Priority) -> bool {
        let inserted =
            Self::priority_set_impl(account, priority, &self.blocked, &mut self.priorities);
        self.trim_overflow();
        inserted
    }

    fn priority_set_impl(
        account: &Account,
        priority: Priority,
        blocking: &BlockedAccounts,
        priorities: &mut PriorityContainer,
    ) -> bool {
        if account.is_zero() || blocking.contains(account) || priorities.contains(account) {
            false
        } else {
            priorities.insert(PriorityEntry::new(*account, priority));
            true
        }
    }

    pub fn priority_erase(&mut self, account: &Account) -> bool {
        if account.is_zero() {
            return false;
        }

        self.priorities.remove(account).is_some()
    }

    pub fn block(&mut self, account: Account, dependency: BlockHash, now: Timestamp) -> bool {
        debug_assert!(!account.is_zero());

        let removed = self.priorities.remove(&account);

        if removed.is_some() {
            self.blocked.insert(BlockedAccount {
                account,
                dependency,
                dependency_account: Account::ZERO,
                added: now,
            });

            self.trim_overflow();
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
                hash == existing.dependency
            } else {
                true
            };

            if hash_matches {
                debug_assert!(!self.priorities.contains(&account));
                self.priorities
                    .insert(PriorityEntry::new(account, Self::PRIORITY_INITIAL));
                self.blocked.remove_account(&account);
                self.trim_overflow();
                return true;
            }
        }

        false
    }

    /// Should be called periodically to remove old entries from the blocking set
    pub fn decay_blocking(&mut self, now: Timestamp) -> usize {
        let cutoff = now - self.config.blocked_decay;
        self.blocked.remove_older_than(cutoff)
    }

    #[allow(dead_code)]
    pub fn last_request(&self, account: &Account) -> Option<Timestamp> {
        self.priorities.get(account).and_then(|i| i.last_request)
    }

    pub fn set_last_request(&mut self, account: &Account, now: Timestamp) {
        debug_assert!(!account.is_zero());
        self.priorities.set_last_request(account, Some(now));
    }

    pub fn reset_last_request(&mut self, account: &Account) {
        debug_assert!(!account.is_zero());

        self.priorities.set_last_request(account, None);
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
            && !self.priority_full()
            && Self::priority_set_impl(
                &dependency_account,
                Self::PRIORITY_INITIAL,
                &self.blocked,
                &mut self.priorities,
            )
        {
            self.trim_overflow();
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

    /// Sampling
    pub fn next_priority(
        &self,
        now: Timestamp,
        filter: impl Fn(&Account) -> bool,
    ) -> PriorityResult {
        if self.priorities.is_empty() {
            return Default::default();
        }

        let cutoff = now - self.config.account_cooldown;

        let Some(entry) = self.priorities.next_priority(cutoff, filter) else {
            return Default::default();
        };

        PriorityResult {
            account: entry.account,
            priority: entry.priority,
            fails: entry.fails,
        }
    }

    pub fn next_blocking(&self, filter: impl Fn(&BlockHash) -> bool) -> BlockHash {
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
            if self.priority_full() {
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
        inserted
    }

    pub fn blocked(&self, account: &Account) -> bool {
        self.blocked.contains(account)
    }

    pub fn prioritized(&self, account: &Account) -> bool {
        self.priorities.contains(account)
    }

    pub fn priority_len(&self) -> usize {
        self.priorities.len()
    }

    pub fn blocked_len(&self) -> usize {
        self.blocked.len()
    }

    pub fn unique_blocking_accounts(&self) -> usize {
        self.blocked.unique_blocked_accounts()
    }

    pub fn known_dependencies(&self) -> usize {
        self.blocked.known_dependencies()
    }

    pub fn iter_priorities(&self) -> impl Iterator<Item = (Priority, &Account)> {
        self.priorities.iter()
    }

    pub fn iter_blocked(&self) -> impl Iterator<Item = &BlockedAccount> {
        self.blocked.iter_by_insertion_order()
    }

    pub fn priority_full(&self) -> bool {
        self.priorities.len() >= self.config.max_prioritized_accounts
    }

    pub fn priority_half_full(&self) -> bool {
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
    }

    pub fn container_info(&self) -> ContainerInfo {
        // Count blocking entries with their dependency account unknown
        let blocking_unknown = self.blocked.count_by_dependency_account(&Account::ZERO);
        [
            (
                "priorities",
                self.priorities.len(),
                PriorityContainer::ELEMENT_SIZE,
            ),
            (
                "blocking",
                self.blocked.len(),
                BlockedAccounts::ELEMENT_SIZE,
            ),
            ("blocking_unknown", blocking_unknown, 0),
        ]
        .into()
    }
}

impl Default for CandidateAccounts {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct PriorityResult {
    pub account: Account,
    pub priority: Priority,
    pub fails: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_blocked() {
        let candidates = CandidateAccounts::default();
        assert_eq!(candidates.blocked(&Account::from(1)), false);
    }

    #[test]
    fn block() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);
        candidates.priority_up(&account);

        candidates.block(account, hash, Timestamp::new_test_instance());

        assert!(candidates.blocked(&account));
        assert_eq!(candidates.priority(&account), Priority::ZERO);
    }

    #[test]
    fn blocking_unknown_account_does_nothing() {
        let mut candidates = CandidateAccounts::default();
        let blocked = candidates.block(
            Account::from(1),
            BlockHash::from(2),
            Timestamp::new_test_instance(),
        );
        assert!(!blocked);
        assert_eq!(candidates.blocked_len(), 0);
    }

    #[test]
    fn unblock() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);
        candidates.priority_up(&account);

        candidates.block(account, hash, Timestamp::new_test_instance());

        assert!(candidates.unblock(account, None));
        assert_eq!(candidates.blocked(&account), false);
    }

    #[test]
    fn unblock_zero_account() {
        let mut candidates = CandidateAccounts::default();
        assert!(!candidates.unblock(Account::ZERO, None));
    }

    #[test]
    fn unblock_unknown_account() {
        let mut candidates = CandidateAccounts::default();
        assert!(!candidates.unblock(Account::from(1), None));
    }

    #[test]
    fn unblock_with_unfulfilled_dependency_does_nothing() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);
        candidates.priority_set_initial(&account);
        candidates.block(account, hash, Timestamp::new_test_instance());

        let unblocked = candidates.unblock(account, Some(BlockHash::from(3)));
        assert!(!unblocked);
        assert!(candidates.blocked(&account));
    }

    #[test]
    fn unblock_with_unknown_dependency_does_nothing() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        let dependency = BlockHash::from(2);
        let unknown_dependency = BlockHash::from(3);
        candidates.priority_set_initial(&account);
        candidates.block(account, dependency, Timestamp::new_test_instance());

        let unblocked = candidates.unblock(account, Some(unknown_dependency));
        assert!(!unblocked);
        assert!(candidates.blocked(&account));
    }

    #[test]
    fn priority_base() {
        let candidates = CandidateAccounts::default();
        assert_eq!(candidates.priority(&Account::from(1)), Priority::ZERO);
    }

    #[test]
    fn priority_unblock() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        let hash = BlockHash::from(2);

        assert_eq!(candidates.priority_up(&account), PriorityUpResult::Inserted);
        assert_eq!(
            candidates.priority(&account),
            CandidateAccounts::PRIORITY_INITIAL
        );

        candidates.block(account, hash, Timestamp::new_test_instance());
        candidates.unblock(account, None);

        assert_eq!(
            candidates.priority(&account),
            CandidateAccounts::PRIORITY_INITIAL
        );
    }

    #[test]
    fn priority_up_down() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);

        candidates.priority_up(&account);
        assert_eq!(
            candidates.priority(&account),
            CandidateAccounts::PRIORITY_INITIAL
        );

        candidates.priority_down(&account);
        assert_eq!(
            candidates.priority(&account),
            CandidateAccounts::PRIORITY_INITIAL / CandidateAccounts::PRIORITY_DIVIDE
        );
    }

    #[test]
    fn priority_down_empty() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);

        candidates.priority_down(&account);

        assert_eq!(candidates.priority(&account), Priority::ZERO);
    }

    // Ensure priority value is bounded
    #[test]
    fn saturate_priority() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);

        for _ in 0..100 {
            candidates.priority_up(&account);
        }
        assert_eq!(
            candidates.priority(&account),
            CandidateAccounts::PRIORITY_MAX
        );
    }

    #[test]
    fn priority_down_saturate() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        candidates.priority_up(&account);
        assert_eq!(
            candidates.priority(&account),
            CandidateAccounts::PRIORITY_INITIAL
        );
        for _ in 0..10 {
            candidates.priority_down(&account);
        }
        assert_eq!(candidates.prioritized(&account), false);
    }

    #[test]
    fn priority_set() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        let prio = Priority::new(10.0);
        candidates.priority_set(&account, prio);
        assert_eq!(candidates.priority(&account), prio);
    }

    #[test]
    fn priority_up_for_zero_account_fails() {
        let mut candidates = CandidateAccounts::default();
        let result = candidates.priority_up(&Account::ZERO);
        assert_eq!(result, PriorityUpResult::InvalidAccount);
        assert_eq!(candidates.blocked_len(), 0);
        assert_eq!(candidates.priority_len(), 0);
    }

    #[test]
    fn priority_up_for_blocked_account_fails() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        candidates.priority_set_initial(&account);
        candidates.block(account, BlockHash::from(2), Timestamp::new_test_instance());
        let result = candidates.priority_up(&account);
        assert_eq!(result, PriorityUpResult::AccountBlocked);
        assert_eq!(candidates.blocked_len(), 1);
    }

    #[test]
    fn priority_down_for_zero_account_fails() {
        let mut candidates = CandidateAccounts::default();
        let result = candidates.priority_down(&Account::ZERO);
        assert_eq!(result, PriorityDownResult::InvalidAccount);
        assert_eq!(candidates.blocked_len(), 0);
        assert_eq!(candidates.priority_len(), 0);
    }

    #[test]
    fn priority_set_for_zero_account_fails() {
        let mut candidates = CandidateAccounts::default();
        let success = candidates.priority_set_initial(&Account::ZERO);
        assert_eq!(success, false);
        assert_eq!(candidates.blocked_len(), 0);
        assert_eq!(candidates.priority_len(), 0);
    }

    #[test]
    fn priority_set_fails_for_blocked_account() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        candidates.priority_set_initial(&account);
        candidates.block(account, BlockHash::from(2), Timestamp::new_test_instance());
        let success = candidates.priority_set(&account, Priority::new(42.0));

        assert_eq!(success, false);
        assert_eq!(candidates.blocked_len(), 1);
        assert_eq!(candidates.priority_len(), 0);
    }

    #[test]
    fn priority_erase() {
        let mut candidates = CandidateAccounts::default();
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        candidates.priority_set_initial(&account1);
        candidates.priority_set_initial(&account2);
        let removed = candidates.priority_erase(&account1);
        assert!(removed);
        assert!(!candidates.prioritized(&account1));
        assert!(candidates.prioritized(&account2));
    }

    #[test]
    fn priority_erase_zero_account() {
        let mut candidates = CandidateAccounts::default();
        assert!(!candidates.priority_erase(&Account::ZERO));
    }

    #[test]
    fn set_last_request_for_unknown_account_does_nothing() {
        let mut candidates = CandidateAccounts::default();
        candidates.set_last_request(&Account::from(1), Timestamp::new_test_instance());
        assert_eq!(candidates.priority_len(), 0);
    }

    #[test]
    fn set_last_request() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        candidates.priority_set_initial(&account);
        let new_timestamp = Timestamp::new_test_instance() + Duration::from_secs(1000);
        candidates.set_last_request(&account, new_timestamp);
        assert_eq!(candidates.last_request(&account), Some(new_timestamp))
    }

    #[test]
    fn timestamp_reset() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        candidates.priority_set_initial(&account);
        candidates.reset_last_request(&account);
        assert_eq!(
            candidates.priorities.get(&account).unwrap().last_request,
            None
        );
    }

    #[test]
    fn trim_priorities_on_overflow() {
        let mut candidates = CandidateAccounts::new(CandidateAccountsConfig {
            max_prioritized_accounts: 2,
            ..Default::default()
        });
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(3);
        candidates.priority_set(&account1, Priority::new(2.0));
        candidates.priority_set(&account2, Priority::new(1.0));
        candidates.priority_set(&account3, Priority::new(3.0));

        assert_eq!(candidates.priority_len(), 2);
        assert!(candidates.prioritized(&account1));
        assert!(candidates.prioritized(&account3));
        assert!(!candidates.prioritized(&account2));
    }

    #[test]
    fn trim_bocked_on_overflow() {
        let mut candidates = CandidateAccounts::new(CandidateAccountsConfig {
            max_blocked_accounts: 2,
            ..Default::default()
        });
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(3);
        candidates.priority_up(&account1);
        candidates.priority_up(&account2);
        candidates.priority_up(&account3);
        candidates.block(account1, BlockHash::from(1), Timestamp::new_test_instance());
        candidates.block(account2, BlockHash::from(2), Timestamp::new_test_instance());
        candidates.block(account3, BlockHash::from(3), Timestamp::new_test_instance());

        assert_eq!(candidates.blocked_len(), 2);
        assert!(candidates.blocked(&account2));
        assert!(candidates.blocked(&account3));
        assert!(!candidates.blocked(&account1));
    }

    #[test]
    fn next_priority_empty() {
        let candidates = CandidateAccounts::default();
        let next = candidates.next_priority(Timestamp::new_test_instance(), |_| true);
        assert_eq!(next, PriorityResult::default());
    }

    #[test]
    fn next_priority() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        candidates.priority_set_initial(&account);
        let now = Timestamp::new_test_instance();
        let next = candidates.next_priority(now, |_| true);
        assert_eq!(
            next,
            PriorityResult {
                account,
                priority: CandidateAccounts::PRIORITY_INITIAL,
                fails: 0
            }
        )
    }

    #[test]
    fn next_priority_none_above_cutoff() {
        let mut candidates = CandidateAccounts::default();
        let now = Timestamp::new_test_instance();
        let account = Account::from(1);
        candidates.priority_up(&account);
        candidates.set_last_request(&account, now);
        let next = candidates.next_priority(now, |_| true);
        assert_eq!(next, PriorityResult::default());
    }

    #[test]
    fn next_priority_cutoff() {
        let config = CandidateAccountsConfig::default();
        let mut candidates = CandidateAccounts::new(config.clone());
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        candidates.priority_set(&account1, Priority::new(100.0));
        candidates.priority_set(&account2, Priority::new(1.0));
        let now = Timestamp::new_test_instance();
        candidates.set_last_request(
            &account1,
            now - config.account_cooldown + Duration::from_millis(1),
        );
        candidates.set_last_request(&account2, now - config.account_cooldown);
        let next = candidates.next_priority(now, |_| true);
        assert_eq!(
            next,
            PriorityResult {
                account: account2,
                priority: Priority::new(1.0),
                fails: 0
            }
        );
    }

    #[test]
    fn next_priority_filter() {
        let config = CandidateAccountsConfig::default();
        let mut candidates = CandidateAccounts::new(config.clone());
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(2);
        candidates.priority_set_initial(&account1);
        candidates.priority_set_initial(&account2);
        candidates.priority_set_initial(&account3);
        let now = Timestamp::new_test_instance();
        let next = candidates.next_priority(now, |a| *a == account2);
        assert_eq!(
            next,
            PriorityResult {
                account: account2,
                priority: CandidateAccounts::PRIORITY_INITIAL,
                fails: 0
            }
        );
    }

    #[test]
    fn next_blocking_empty() {
        let candidates = CandidateAccounts::default();
        assert_eq!(candidates.next_blocking(|_| true), BlockHash::ZERO);
    }

    #[test]
    fn next_blocking() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        let dependency = BlockHash::from(2);
        candidates.priority_set_initial(&account);
        candidates.block(account, dependency, Timestamp::new_test_instance());
        assert_eq!(candidates.next_blocking(|_| true), dependency);
    }

    #[test]
    fn next_blocking_filter() {
        let mut candidates = CandidateAccounts::default();
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        let account3 = Account::from(3);
        let dependency = BlockHash::from(2);
        candidates.priority_set_initial(&account1);
        candidates.priority_set_initial(&account2);
        candidates.priority_set_initial(&account3);
        candidates.block(
            account1,
            BlockHash::from(1000),
            Timestamp::new_test_instance(),
        );
        candidates.block(account2, dependency, Timestamp::new_test_instance());
        candidates.block(
            account3,
            BlockHash::from(2000),
            Timestamp::new_test_instance(),
        );
        assert_eq!(candidates.next_blocking(|h| *h == dependency), dependency);
    }

    #[test]
    fn sync_dependencies_empty() {
        let mut candidates = CandidateAccounts::default();
        let inserted = candidates.sync_dependencies();
        assert_eq!(inserted, 0);
    }

    #[test]
    fn sync_dependencies_insert_one_account() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        candidates.priority_set_initial(&account);
        candidates.block(account, dependency, Timestamp::new_test_instance());

        candidates.dependency_update(&dependency, dependency_account);

        assert!(candidates.prioritized(&dependency_account));
    }

    #[test]
    fn sync_dependencies_doesnt_insert_when_dependency_account_already_prioritized() {
        let mut candidates = CandidateAccounts::default();
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        candidates.priority_set_initial(&account);
        candidates.block(account, dependency, Timestamp::new_test_instance());
        candidates.dependency_update(&dependency, dependency_account);
        candidates.priority_set_initial(&dependency_account);

        let inserted = candidates.sync_dependencies();

        assert_eq!(inserted, 0);
    }

    #[test]
    fn sync_dependencies_doesnt_insert_when_max_accounts_prioritized() {
        let config = CandidateAccountsConfig {
            max_prioritized_accounts: 2,
            ..Default::default()
        };
        let mut candidates = CandidateAccounts::new(config);
        let account = Account::from(1);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        candidates.priority_set_initial(&account);
        candidates.block(account, dependency, Timestamp::new_test_instance());
        candidates.dependency_update(&dependency, dependency_account);
        candidates.priority_set_initial(&Account::from(9999));
        candidates.priority_set_initial(&Account::from(8888));

        let inserted = candidates.sync_dependencies();

        assert_eq!(inserted, 0);
    }

    #[test]
    fn blocked_half_full() {
        let config = CandidateAccountsConfig {
            max_blocked_accounts: 3,
            ..Default::default()
        };
        let mut candidates = CandidateAccounts::new(config);
        let account1 = Account::from(1);
        let account2 = Account::from(2);

        assert!(!candidates.blocked_half_full());

        candidates.priority_set_initial(&account1);
        candidates.block(account1, BlockHash::from(1), Timestamp::new_test_instance());
        assert!(!candidates.blocked_half_full());

        candidates.priority_set_initial(&account2);
        candidates.block(account2, BlockHash::from(2), Timestamp::new_test_instance());
        assert!(candidates.blocked_half_full());
    }

    #[test]
    fn container_info() {
        let mut candidates = CandidateAccounts::default();
        candidates.priority_set_initial(&Account::from(1));
        candidates.priority_set_initial(&Account::from(2));
        candidates.priority_set_initial(&Account::from(3));
        candidates.block(
            Account::from(2),
            BlockHash::from(3),
            Timestamp::new_test_instance(),
        );
        candidates.dependency_update(&BlockHash::from(3), Account::from(1000));
        candidates.block(
            Account::from(3),
            BlockHash::from(4),
            Timestamp::new_test_instance(),
        );
        let info = candidates.container_info();
        assert_eq!(
            info,
            [
                ("priorities", 2, PriorityContainer::ELEMENT_SIZE),
                ("blocking", 2, BlockedAccounts::ELEMENT_SIZE),
                ("blocking_unknown", 1, 0)
            ]
            .into()
        )
    }
}
