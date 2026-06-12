use std::{collections::VecDeque, time::Duration};

use rustc_hash::{FxHashMap, FxHashSet};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::{Account, Block, BlockHash};
use rsnano_utils::container_info::{ContainerInfo, ContainerInfoProvider};

use super::{
    Priority, PriorityDownResult, PriorityUpResult, PullType,
    account_priority_tracker::AccountPriorityTracker,
    block_handoff_queue::{BlockHandoffQueue, ProcessingFinished},
    blocked::BlockedAccounts,
    download_queue::DownloadQueue,
    downloading::DownloadingAccounts,
};
use rsnano_network::ChannelId;

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

#[derive(Debug, PartialEq, Eq)]
pub struct DownloadTarget {
    pub account: Account,
    pub priority: Priority,
    pub pull_type: PullType,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BootstrapQueueConfig {
    pub max_unblocked_accounts: usize,
    pub max_blocked_accounts: usize,
    pub max_ready_to_process: usize,

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
            max_ready_to_process: 8 * 1024,
            blocked_decay: Duration::from_hours(1),
            account_cooldown: Duration::from_secs(3),
        }
    }
}

#[derive(Default)]
pub(crate) struct TrimCount {
    pub download_queue: usize,
    pub blocked: usize,
}

struct FailEntry {
    pub fail_count: usize,
    pub last_failed_channel: ChannelId,
}

/// A prioritized queue of accounts which should bootstrapped.
/// Accounts can be blocked, because a dependency block is missing. Blocked accounts
/// are put on hold.
pub(crate) struct BootstrapQueueLogic {
    config: BootstrapQueueConfig,
    priorities: AccountPriorityTracker,
    fails: FxHashMap<Account, FailEntry>,
    download_queue: DownloadQueue,
    downloading: DownloadingAccounts,
    block_processing: BlockHandoffQueue,
    blocked: BlockedAccounts,
    safe_accounts: FxHashSet<Account>,
    revision: u64,
    discarded_blocks: usize,
}

impl BootstrapQueueLogic {
    pub const MAX_FAILS: usize = 2;

    pub fn new(config: BootstrapQueueConfig) -> Self {
        Self {
            config,
            priorities: Default::default(),
            fails: Default::default(),
            download_queue: Default::default(),
            blocked: Default::default(),
            downloading: Default::default(),
            block_processing: Default::default(),
            safe_accounts: Default::default(),
            revision: 0,
            discarded_blocks: 0,
        }
    }

    pub fn insert(&mut self, account: Account, priority: Priority) -> bool {
        if self.priorities.insert(account, priority) {
            self.download_queue.insert(account, priority);
            true
        } else {
            false
        }
    }

    pub fn insert_pull_type(
        &mut self,
        account: Account,
        priority: Priority,
        pull_type: PullType,
    ) -> bool {
        let inserted = self.insert(account, priority);
        if pull_type == PullType::Safe && self.priorities.contains(&account) {
            self.safe_accounts.insert(account);
        }
        inserted
    }

    pub fn pull_type(&self, account: &Account) -> PullType {
        if self.safe_accounts.contains(account) {
            PullType::Safe
        } else {
            PullType::Optimistic
        }
    }

    pub fn priority_up(&mut self, account: &Account) -> PriorityUpResult {
        let result = self.priorities.priority_up(account);
        if let PriorityUpResult::Upgraded(_, new_prio) = result {
            self.download_queue.change_priority(account, new_prio);
        }

        self.revision += 1;
        result
    }

    pub fn priority_down(&mut self, account: &Account) -> PriorityDownResult {
        let result = self.priorities.priority_down(account);
        match result {
            PriorityDownResult::Deprioritized(_, new_prio) => {
                self.download_queue.change_priority(account, new_prio);
            }
            PriorityDownResult::Removed => {
                self.remove(account);
            }
            _ => {}
        }

        self.revision += 1;
        result
    }

    pub fn remove(&mut self, account: &Account) -> bool {
        let mut to_remove = VecDeque::new();
        to_remove.push_back(*account);
        while let Some(account) = to_remove.pop_front() {
            self.priorities.remove(&account);
            self.fails.remove(&account);
            self.download_queue.remove(&account);
            self.downloading.remove(&account);
            self.safe_accounts.remove(&account);
            to_remove.extend(self.blocked.remove_account_and_dependents(&account));
            let discarded = self.block_processing.remove(&account);
            self.discarded_blocks += discarded;
        }
        self.revision += 1;
        true
    }

    pub fn block(&mut self, block_hash: &BlockHash, dependency: BlockHash, now: Timestamp) -> bool {
        if let Some(account) = self.block_processing.processing_failed(&block_hash) {
            self.blocked.insert(account, dependency, now);
            self.revision += 1;
            true
        } else {
            false
        }
    }

    pub fn unblock_hash(&mut self, dependency_hash: &BlockHash) -> bool {
        let accounts = self.blocked.unblock(dependency_hash);
        if accounts.is_empty() {
            return false;
        }

        for account in accounts {
            let first_hash = self.block_processing.resume(account);
            if first_hash.is_none() {
                let priority = self.priorities.get(&account).unwrap();
                self.download_queue.insert(account, priority);
            }
        }

        self.revision += 1;
        true
    }

    pub fn unblock(&mut self, account: Account) -> bool {
        if !self.blocked.remove(&account) {
            return false;
        }
        let first_hash = self.block_processing.resume(account);
        if first_hash.is_none() {
            let priority = self.priorities.get(&account).unwrap();
            self.download_queue.insert(account, priority);
        }

        self.revision += 1;
        true
    }

    pub fn dependency_account_requested(&mut self, dependency: &BlockHash) {
        self.blocked.dependency_account_requested(dependency);
    }

    pub fn dependency_account_not_found(&mut self, dependency: &BlockHash) {
        self.blocked.remove_dependency_request(dependency);
        self.revision += 1;
    }

    /// Removes the request mark for a dependency, so that the dependency
    /// can be requested again
    pub fn remove_dependency_request(&mut self, dependency: &BlockHash) -> bool {
        let removed = self.blocked.remove_dependency_request(dependency);
        if removed {
            self.revision += 1;
        }
        removed
    }

    /// Sets information about the account chain that contains the block hash.
    /// Returns the number of updated accounts.
    pub fn dependency_update(
        &mut self,
        dependency: &BlockHash,
        dependency_account: Account,
    ) -> (usize, bool) {
        let updated = self
            .blocked
            .modify_dependency_account(dependency, dependency_account)
            .len();

        let dep_inserted = if updated > 0 && !self.queue_full() {
            self.insert(dependency_account, Priority::INITIAL)
        } else {
            false
        };
        self.revision += 1;

        (updated, dep_inserted)
    }

    /// Erase the oldest entries
    pub fn trim_overflow(&mut self) -> TrimCount {
        let mut trim_count = TrimCount {
            download_queue: 0,
            blocked: 0,
        };

        while self.needs_trimming() {
            let account = self.download_queue.pop_lowest_prio().unwrap();
            self.remove(&account);
            trim_count.download_queue += 1;
        }

        while self.blocked.len() > self.config.max_blocked_accounts {
            let to_remove = *self.blocked.oldest().unwrap();
            self.remove(&to_remove);
            trim_count.blocked += 1;
        }

        trim_count
    }

    fn needs_trimming(&self) -> bool {
        !self.download_queue.is_empty()
            && self.unblocked_count() > self.config.max_unblocked_accounts
    }

    pub fn next_download_target(&self) -> Option<DownloadTarget> {
        if self.block_processing.ready_to_process_len() > self.config.max_ready_to_process {
            return None;
        }
        self.download_queue
            .iter()
            .next()
            .map(|(priority, account)| DownloadTarget {
                account: *account,
                priority,
                pull_type: self.pull_type(account),
            })
    }

    pub fn take_next_block_for_processing(&mut self) -> Option<Block> {
        self.block_processing.take_next_block_for_processing()
    }

    pub fn revert_processing_started(&mut self, block_hash: &BlockHash) -> bool {
        self.block_processing.revert_processing_started(block_hash)
    }

    pub fn download_started(&mut self, account: &Account) -> bool {
        if !self.download_queue.remove(account) {
            return false;
        }
        self.downloading.insert(*account);
        true
    }

    /// Puts a downloading account back into the download queue, so that it
    /// can be requested again
    pub fn requeue_download(&mut self, account: &Account) -> bool {
        if !self.downloading.remove(account) {
            return false;
        }
        let priority = self.priorities.get(account).unwrap();
        self.download_queue.insert(*account, priority);
        self.revision += 1;
        true
    }

    pub fn download_finished(
        &mut self,
        account: &Account,
        blocks: VecDeque<Block>,
        should_cool_down: bool,
        channel_id: ChannelId,
    ) -> (bool, Option<ChannelId>) {
        let mut out_of_date_channel = None;
        if !self.downloading.remove(account) {
            return (false, out_of_date_channel);
        }

        let was_safe = self.safe_accounts.remove(account);

        let fails = if should_cool_down {
            let fails = self.fails.entry(*account).or_insert_with(|| FailEntry {
                fail_count: 0,
                last_failed_channel: channel_id,
            });
            if fails.fail_count > 0 && !blocks.is_empty() && !was_safe {
                out_of_date_channel = Some(fails.last_failed_channel);
            }
            fails.fail_count += 1;
            fails.last_failed_channel = channel_id;
            fails.fail_count
        } else {
            if let Some(failed) = self.fails.remove(account) {
                if !was_safe {
                    out_of_date_channel = Some(failed.last_failed_channel);
                }
            }
            0
        };

        if blocks.is_empty() {
            // try to reenqueue for download
            if fails >= Self::MAX_FAILS {
                // TODO stats
                self.remove(account);
            } else {
                let priority = self.priorities.get(account).unwrap();
                self.download_queue.insert(*account, priority);
            }
        } else {
            self.block_processing.enqueue(*account, blocks);
        }

        (true, out_of_date_channel)
    }

    pub fn reprocess(&mut self, block_hash: &BlockHash) -> bool {
        self.block_processing.reprocess(block_hash)
    }

    pub fn processing_finished(&mut self, block_hash: &BlockHash) -> bool {
        let ProcessingFinished {
            account,
            next_block_hash,
        } = match self.block_processing.processing_finished(block_hash) {
            Some(result) => result,
            None => return false,
        };

        if next_block_hash.is_none() {
            let priority = self.priorities.get(&account).unwrap();
            self.download_queue.insert(account, priority);
        }
        true
    }

    pub fn processing_failed(&mut self, block_hash: &BlockHash) {
        let Some(account) = self.block_processing.processing_failed(block_hash) else {
            return;
        };
        self.remove(&account);
    }

    pub fn next_unknown_blocking_hash(&self) -> Option<BlockHash> {
        self.blocked.next_unknown_blocking_hash()
    }

    /// Enqueues dependency accounts for all blocked accounts whose dependency is known.
    /// Returns the number of inserted accounts.
    pub fn sync_dependencies(&mut self) -> usize {
        if self.queue_full() {
            return 0;
        }

        let mut accounts_to_enqueue = Vec::new();
        for (dep_account, _blocked_account) in self.blocked.iter_known_dep_accounts() {
            if self.queue_full() {
                break;
            }
            if !self.contains(&dep_account) {
                accounts_to_enqueue.push(dep_account);
            }
        }

        let mut inserted = 0;
        for account in accounts_to_enqueue {
            if self.queue_full() {
                break;
            }
            self.insert(account, Priority::INITIAL);
            inserted += 1;
        }

        inserted
    }

    #[cfg(test)]
    pub fn blocked(&self, account: &Account) -> bool {
        self.blocked.contains(account)
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.priorities.contains(account)
    }

    pub fn unblocked_count(&self) -> usize {
        self.priorities.len() - self.blocked.len()
    }

    pub fn snapshot(&self, limit: usize, filter: Option<Account>) -> BootstrapQueueSnapshot {
        let download_queue = self
            .iter_download_queue()
            .filter(|(account, _)| filter.is_none() || filter == Some(*account))
            .take(limit)
            .map(|(account, priority)| BootstrappingAccountInfo {
                account,
                priority,
                dependency_block: BlockHash::ZERO,
                dependency_account: Account::ZERO,
            })
            .collect();

        let downloading = self
            .iter_downloading()
            .filter(|(account, _)| filter.is_none() || filter == Some(*account))
            .take(limit)
            .map(|(account, priority)| BootstrappingAccountInfo {
                account,
                priority,
                dependency_block: BlockHash::ZERO,
                dependency_account: Account::ZERO,
            })
            .collect();

        let blocked = self
            .iter_blocked()
            .filter(|(account, _)| {
                if filter.is_none() || filter == Some(*account) {
                    return true;
                }
                self.blocked
                    .get_info(account)
                    .and_then(|(_, dep_account)| dep_account)
                    .map(|a| Some(a) == filter)
                    .unwrap_or(false)
            })
            .take(limit)
            .map(|(account, priority)| {
                let (dependency_block, dependency_account) = self
                    .blocked
                    .get_info(&account)
                    .map(|(h, a)| (h, a.unwrap_or_default()))
                    .unwrap_or_default();
                BootstrappingAccountInfo {
                    account,
                    priority,
                    dependency_block,
                    dependency_account,
                }
            })
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
            ready_to_process: self.block_processing.ready_to_process_len(),
            processing: self.block_processing.processing_len(),
            blocked: self.blocked.len(),
            unknown_dependencies: self.blocked.len() - self.blocked.known_dependencies(),
            unique_blocking_accounts: self.blocked.unique_dependency_accounts(),
            cached_blocks: self.block_processing.cached_block_count(),
            discarded_blocks: self.discarded_blocks,
        }
    }

    fn iter_download_queue(&self) -> impl Iterator<Item = (Account, Priority)> + '_ {
        self.download_queue
            .iter()
            .map(|(prio, account)| (*account, prio))
    }

    fn iter_downloading(&self) -> impl Iterator<Item = (Account, Priority)> + '_ {
        self.downloading
            .iter_accounts()
            .map(|account| (*account, self.priorities.get(account).unwrap()))
    }

    fn iter_blocked(&self) -> impl Iterator<Item = (Account, Priority)> + '_ {
        self.blocked
            .iter_by_timestamp()
            .map(|account| (*account, self.priorities.get(account).unwrap()))
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
        self.priorities.get(account).unwrap_or(Priority::ZERO)
    }

    pub fn clear_blocked_accounts(&mut self) {
        let to_remove: Vec<_> = self.blocked.iter_by_timestamp().copied().collect();
        for account in to_remove {
            self.remove(&account);
        }
        self.revision += 1;
    }

    pub fn missing_sends(&self) -> Vec<BlockHash> {
        self.blocked.missing_sends().cloned().collect()
    }

    pub fn processing(&self) -> Vec<BlockHash> {
        self.block_processing.processing()
    }

    /// Should be called periodically to remove old entries from the blocked accounts
    pub fn decay_blocked_accounts(&mut self, now: Timestamp) -> usize {
        let cutoff = now - self.config.blocked_decay;
        self.revision += 1;
        let removed = self.blocked.remove_older_than(cutoff);
        for account in &removed {
            self.remove(account);
        }
        removed.len()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn container_info(&self) -> ContainerInfo {
        let blocked_unknown = self.blocked.count_by_dependency_account(&Account::ZERO);
        ContainerInfo::builder()
            .leaf("download_queue", self.download_queue.len(), 0)
            .leaf("blocked", self.blocked.len(), 0)
            .leaf("blocked_unknown", blocked_unknown, 0)
            .leaf("unblocked", self.unblocked_count(), 0)
            .leaf("downloading", self.downloading.len(), 0)
            .leaf("optimistic", self.safe_accounts.len(), 0)
            .leaf("fails", self.fails.len(), 0)
            .node("processing", self.block_processing.container_info())
            .node("priorities", self.priorities.container_info())
            .node(
                "download_queue_detail",
                self.download_queue.container_info(),
            )
            .node("downloading_detail", self.downloading.container_info())
            .node("blocked_detail", self.blocked.container_info())
            .finish()
    }
}

impl Default for BootstrapQueueLogic {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{PrivateKey, StateBlockArgs};

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

        queue.insert(account, prio);

        assert!(queue.contains(&account));
        assert_eq!(queue.priority(&account), prio);
    }

    #[test]
    fn priority_can_be_changed_for_blocked_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account_key = PrivateKey::from(42);
        let account = account_key.account();
        let dependency = BlockHash::from(2);
        make_blocked_account(&mut queue, &account_key, dependency);
        let old_prio = queue.priority(&account);
        queue.priority_up(&account);

        assert_eq!(queue.priority(&account), old_prio.increase());
        assert_eq!(queue.info().blocked, 1);
        assert_eq!(queue.info().download_queue, 0);
    }

    #[test]
    fn account_that_isnt_in_the_queue_has_no_priority() {
        let queue = BootstrapQueueLogic::default();
        assert_eq!(queue.priority(&Account::from(1)), Priority::ZERO);
    }

    #[test]
    fn priority_cant_be_changed_if_account_isnt_enqueued() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.priority_up(&account);
        assert!(!queue.contains(&account));
        assert_eq!(queue.priority(&account), Priority::ZERO);
    }

    /*
     * Increasing priority
     */

    #[test]
    fn priority_has_an_upper_limit() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);

        queue.insert(account, Priority::INITIAL);
        for _ in 0..100 {
            queue.priority_up(&account);
        }

        assert_eq!(queue.priority(&account), Priority::MAX);
    }

    #[test]
    fn zero_account_cant_be_prioritized() {
        let mut queue = BootstrapQueueLogic::default();
        assert!(!queue.insert(Account::ZERO, Priority::INITIAL));
        assert_eq!(queue.info().blocked, 0);
        assert_eq!(queue.info().download_queue, 0);
    }

    #[test]
    fn priority_can_be_increased_for_blocked_account() {
        let mut queue = BootstrapQueueLogic::default();
        let key = PrivateKey::from(42);
        let dependency = BlockHash::from(2);
        make_blocked_account(&mut queue, &key, dependency);

        let result = queue.priority_up(&key.account());

        assert!(matches!(result, PriorityUpResult::Upgraded(_, _)));
        assert_eq!(queue.info().blocked, 1);
        assert_eq!(
            queue.priority(&key.account()),
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
        queue.insert(account, Priority::INITIAL);

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
        queue.insert(account, Priority::INITIAL);

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
        let key = PrivateKey::from(42);
        let dependency = BlockHash::from(2);
        make_blocked_account(&mut queue, &key, dependency);
        assert!(queue.blocked(&key.account()));
        assert_eq!(queue.info().download_queue, 0);
        assert_eq!(queue.info().blocked, 1);
    }

    #[test]
    fn blocking_unknown_account_does_nothing() {
        let mut queue = BootstrapQueueLogic::default();
        let hash = BlockHash::from(2);
        let blocked = queue.block(&hash, BlockHash::from(2), Timestamp::new_test_instance());
        assert!(!blocked);
        assert_eq!(queue.info().blocked, 0);
    }

    /*
     * Unblocking an account
     */

    #[test]
    fn unblock_unblocks_the_account() {
        let mut queue = BootstrapQueueLogic::default();
        let key = PrivateKey::from(42);
        let dependency = BlockHash::from(2);
        let account = key.account();
        make_blocked_account(&mut queue, &key, dependency);

        assert!(queue.unblock_hash(&dependency));

        assert_eq!(queue.blocked(&account), false);
        assert_eq!(queue.info().ready_to_process, 1);
    }

    #[test]
    fn unblock_unknown_dependency() {
        let mut queue = BootstrapQueueLogic::default();
        let dependency = BlockHash::from(1);
        assert!(!queue.unblock_hash(&dependency));
    }

    #[test]
    fn priority_stays_unchanged_after_unblock() {
        let mut queue = BootstrapQueueLogic::default();
        let key = PrivateKey::from(1);
        let hash = BlockHash::from(2);
        let priority = Priority::new(99.0);
        queue.insert(key.account(), priority);
        make_blocked_account(&mut queue, &key, hash);

        queue.unblock_hash(&hash);

        assert_eq!(queue.priority(&key.account()), priority);
    }

    /*
     * Insert optimistic
     */

    #[test]
    fn insert_safe_inserts_new_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);

        let inserted = queue.insert_pull_type(account, Priority::INITIAL, PullType::Safe);

        assert!(inserted);
        let target = queue.next_download_target().unwrap();
        assert_eq!(target.account, account);
        assert_eq!(target.pull_type, PullType::Safe);
    }

    #[test]
    fn insert_safe_promotes_existing_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.insert(account, Priority::INITIAL);
        assert_eq!(queue.pull_type(&account), PullType::Optimistic);

        let inserted = queue.insert_pull_type(account, Priority::INITIAL, PullType::Safe);

        assert!(!inserted);
        assert_eq!(queue.pull_type(&account), PullType::Safe);
    }

    #[test]
    fn download_finished_resets_pull_type_to_optimistic() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.insert_pull_type(account, Priority::INITIAL, PullType::Safe);
        queue.download_started(&account);

        queue.download_finished(&account, VecDeque::new(), false, ChannelId::from(1));

        assert_eq!(queue.pull_type(&account), PullType::Optimistic);
    }

    #[test]
    fn download_finished_no_op_does_not_reset_flag() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.insert_pull_type(account, Priority::INITIAL, PullType::Safe);

        let (finished, _) =
            queue.download_finished(&account, VecDeque::new(), false, ChannelId::from(1));

        assert!(!finished);
        assert_eq!(queue.pull_type(&account), PullType::Safe);
    }

    #[test]
    fn remove_clears_pull_type() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.insert_pull_type(account, Priority::INITIAL, PullType::Safe);

        queue.remove(&account);

        assert_eq!(queue.pull_type(&account), PullType::Optimistic);
    }

    /*
     * Requeue download
     */

    #[test]
    fn requeue_download_puts_account_back_into_download_queue() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.insert(account, Priority::INITIAL);
        queue.download_started(&account);
        assert_eq!(queue.next_download_target(), None);

        let requeued = queue.requeue_download(&account);

        assert!(requeued);
        assert_eq!(queue.info().downloading, 0);
        assert_eq!(queue.next_download_target().unwrap().account, account);
    }

    #[test]
    fn requeue_download_keeps_priority() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        let priority = Priority::new(99.0);
        queue.insert(account, priority);
        queue.download_started(&account);

        queue.requeue_download(&account);

        assert_eq!(queue.next_download_target().unwrap().priority, priority);
    }

    #[test]
    fn requeue_download_for_unknown_account_does_nothing() {
        let mut queue = BootstrapQueueLogic::default();

        let requeued = queue.requeue_download(&Account::from(1));

        assert!(!requeued);
        assert_eq!(queue.info().download_queue, 0);
    }

    /*
     * Remove dependency request
     */

    #[test]
    fn remove_dependency_request_makes_dependency_requestable_again() {
        let mut queue = BootstrapQueueLogic::default();
        let key = PrivateKey::from(42);
        let dependency = BlockHash::from(2);
        make_blocked_account(&mut queue, &key, dependency);
        queue.dependency_account_requested(&dependency);
        assert_eq!(queue.next_unknown_blocking_hash(), None);

        let removed = queue.remove_dependency_request(&dependency);

        assert!(removed);
        assert_eq!(queue.next_unknown_blocking_hash(), Some(dependency));
    }

    #[test]
    fn remove_dependency_request_for_unknown_dependency_does_nothing() {
        let mut queue = BootstrapQueueLogic::default();

        let removed = queue.remove_dependency_request(&BlockHash::from(1));

        assert!(!removed);
    }

    /*
     * Misc
     */

    #[test]
    fn remove_removes_the_account() {
        let mut queue = BootstrapQueueLogic::default();
        let account1 = Account::from(1);
        let account2 = Account::from(2);
        queue.insert(account1, Priority::INITIAL);
        queue.insert(account2, Priority::INITIAL);
        let removed = queue.remove(&account1);
        assert!(removed);
        assert!(!queue.contains(&account1));
        assert!(queue.contains(&account2));
    }

    #[test]
    fn next_priority_empty() {
        let queue = BootstrapQueueLogic::default();
        let next = queue.next_download_target();
        assert_eq!(next, None);
    }

    #[test]
    fn next_priority() {
        let mut queue = BootstrapQueueLogic::default();
        let account = Account::from(1);
        queue.insert(account, Priority::INITIAL);
        let target = queue.next_download_target().unwrap();
        assert_eq!(target.account, account);
        assert_eq!(target.priority, Priority::INITIAL);
        assert_eq!(target.pull_type, PullType::Optimistic);
    }

    #[test]
    fn next_blocked_empty() {
        let queue = BootstrapQueueLogic::default();
        assert_eq!(queue.next_unknown_blocking_hash(), None);
    }

    #[test]
    fn next_blocked() {
        let mut queue = BootstrapQueueLogic::default();
        let key = PrivateKey::from(1);
        let dependency = BlockHash::from(2);
        make_blocked_account(&mut queue, &key, dependency);
        assert_eq!(queue.next_unknown_blocking_hash(), Some(dependency));
    }

    #[test]
    fn next_blocked_filter() {
        let mut queue = BootstrapQueueLogic::default();
        let key1 = PrivateKey::from(1);
        let key2 = PrivateKey::from(2);
        let key3 = PrivateKey::from(3);
        let dependency1 = BlockHash::from(1000);
        let dependency2 = BlockHash::from(2);
        let dependency3 = BlockHash::from(2000);
        make_blocked_account(&mut queue, &key1, dependency1);
        make_blocked_account(&mut queue, &key2, dependency2);
        make_blocked_account(&mut queue, &key3, dependency3);
        queue.dependency_account_requested(&dependency1);
        queue.dependency_account_requested(&dependency3);
        assert_eq!(queue.next_unknown_blocking_hash(), Some(dependency2));
    }

    #[test]
    fn blocked_half_full() {
        let config = BootstrapQueueConfig {
            max_blocked_accounts: 3,
            ..Default::default()
        };
        let mut queue = BootstrapQueueLogic::new(config);
        let key1 = PrivateKey::from(1);
        let key2 = PrivateKey::from(2);
        assert!(!queue.blocked_half_full());

        make_blocked_account(&mut queue, &key1, BlockHash::from(1));
        assert!(!queue.blocked_half_full());

        make_blocked_account(&mut queue, &key2, BlockHash::from(2));
        assert!(queue.blocked_half_full());
    }

    #[test]
    fn container_info() {
        let mut queue = BootstrapQueueLogic::default();
        queue.insert(Account::from(1), Priority::INITIAL);
        queue.insert(Account::from(2), Priority::INITIAL);
        let info = queue.container_info();
        assert_eq!(info.leaf("download_queue"), Some(2));
        assert_eq!(info.leaf("blocked"), Some(0));
        assert_eq!(info.leaf("blocked_unknown"), Some(0));
        assert_eq!(info.leaf("unblocked"), Some(2));
        assert_eq!(info.leaf("downloading"), Some(0));
        assert_eq!(
            info.node("processing").unwrap(),
            &queue.block_processing.container_info()
        );
        assert_eq!(
            info.node("priorities").unwrap(),
            &queue.priorities.container_info()
        );
        assert_eq!(
            info.node("download_queue_detail").unwrap(),
            &queue.download_queue.container_info()
        );
        assert_eq!(
            info.node("downloading_detail").unwrap(),
            &queue.downloading.container_info()
        );
        assert_eq!(
            info.node("blocked_detail").unwrap(),
            &queue.blocked.container_info()
        );
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
        let key = PrivateKey::from(42);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        make_blocked_account(&mut queue, &key, dependency);

        queue.dependency_update(&dependency, dependency_account);

        assert!(queue.contains(&dependency_account));
    }

    #[test]
    fn sync_dependencies_doesnt_insert_when_dependency_account_already_prioritized() {
        let mut queue = BootstrapQueueLogic::default();
        let key = PrivateKey::from(42);
        let dependency_account = Account::from(2);
        let dependency = BlockHash::from(100);
        make_blocked_account(&mut queue, &key, dependency);
        queue.dependency_update(&dependency, dependency_account);

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

        let key = PrivateKey::from(42);
        let dependency = BlockHash::from(100);
        let dependency_account = Account::from(2);
        make_blocked_account(&mut queue, &key, dependency);
        queue.dependency_update(&dependency, dependency_account);

        queue.insert(Account::from(9999), Priority::INITIAL);
        queue.insert(Account::from(8888), Priority::INITIAL);

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

        queue.insert(queued, Priority::INITIAL);
        queue.insert(downloading, Priority::INITIAL);
        queue.download_started(&downloading);

        let snap = queue.snapshot(10, None);

        assert_eq!(snap.info.download_queue, 1);
        assert_eq!(snap.download_queue.len(), 1);
        assert_eq!(snap.download_queue[0].account, queued);
        assert_eq!(snap.download_queue[0].priority, Priority::INITIAL);

        assert_eq!(snap.downloading.len(), 1);
        assert_eq!(snap.downloading[0].account, downloading);
        assert_eq!(snap.downloading[0].priority, Priority::INITIAL);
    }

    /*
     * Test Helpers
     */
    fn make_blocked_account(
        queue: &mut BootstrapQueueLogic,
        key: &PrivateKey,
        dependency: BlockHash,
    ) {
        let now = Timestamp::new_test_instance();
        make_blocked_account_at(queue, key, dependency, now);
    }

    fn make_blocked_account_at(
        queue: &mut BootstrapQueueLogic,
        key: &PrivateKey,
        dependency: BlockHash,
        blocked_at: Timestamp,
    ) {
        let account = key.account();
        let receive: Block = StateBlockArgs {
            key,
            ..StateBlockArgs::new_test_instance()
        }
        .into();
        queue.insert(account, Priority::INITIAL);
        queue.download_started(&account);
        queue.download_finished(&account, [receive].into(), false, ChannelId::from(1));
        let next = queue.take_next_block_for_processing().unwrap();
        queue.block(&next.hash(), dependency, blocked_at);
    }
}
