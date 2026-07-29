mod account_priority_tracker;

mod block_handoff_queue;
mod blocked;
mod download_queue;
mod logic;
mod priority;
mod pull_type;
mod stats;

pub use account_priority_tracker::{PriorityDownResult, PriorityUpResult};
pub use logic::{
    BootstrapQueueConfig, BootstrapQueueInfo, BootstrapQueueSnapshot, BootstrappingAccountInfo,
    DownloadTarget,
};
pub use priority::Priority;
pub use pull_type::PullType;

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

use crate::bootstrap::bootstrapper::bootstrap_queue::logic::TrimCount;
use rsnano_network::ChannelId;
use stats::BootstrapQueueStats;
use tracing::trace;

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

    pub fn insert(&self, account: Account) -> bool {
        self.insert_pull_type(account, PullType::Optimistic)
    }

    /// Inserts an account or updates the already inserted account to
    /// use the safe pull strategy
    pub fn insert_safe(&self, account: Account) -> bool {
        self.insert_pull_type(account, PullType::Safe)
    }

    pub fn insert_safe_on_channel(&self, account: Account, channel_id: ChannelId) -> bool {
        let inserted = self.insert_pull_type(account, PullType::Safe);
        let mut logic = self.logic.lock().unwrap();
        logic.set_preferred_channel(account, channel_id);
        logic.promote_recovery(&account);
        inserted
    }

    fn insert_pull_type(&self, account: Account, pull_type: PullType) -> bool {
        let inserted;
        let mut trim_count = TrimCount::default();
        {
            let mut logic = self.logic.lock().unwrap();
            inserted = logic.insert_pull_type(account, Priority::INITIAL, pull_type);
            if inserted {
                trim_count = logic.trim_overflow();
            }
        }
        if inserted {
            trace!(account = account.encode_account(), "Account inserted");
            self.stats.inserted.fetch_add(1, Relaxed);
            self.stats.add_trim_count(&trim_count);
        }
        inserted
    }

    #[cfg(test)]
    pub fn priority(&self, account: &Account) -> Priority {
        self.logic.lock().unwrap().priority(account)
    }

    pub fn block(&self, block_hash: &BlockHash, dependency: BlockHash) {
        let now = self.clock.now();
        let blocked;
        let trim_count;
        {
            let mut logic = self.logic.lock().unwrap();
            blocked = logic.block(block_hash, dependency, now);
            trim_count = logic.trim_overflow();
        }
        if blocked {
            trace!(
                block = block_hash.encode_hex(),
                dependency = dependency.encode_hex(),
                "Blocked"
            );
            self.stats.blocked.fetch_add(1, Relaxed);
        } else {
            self.stats.block_failed.fetch_add(1, Relaxed);
        }
        self.stats.add_trim_count(&trim_count);
    }

    #[cfg(test)]
    pub fn blocked(&self, account: &Account) -> bool {
        self.logic.lock().unwrap().blocked(account)
    }

    pub fn unblock_hash(&self, dependency_hash: &BlockHash) {
        let unblocked;
        let trim_count;
        {
            let mut logic = self.logic.lock().unwrap();
            unblocked = logic.unblock_hash(dependency_hash);
            trim_count = logic.trim_overflow();
        }
        if unblocked {
            trace!(dependency = dependency_hash.encode_hex(), "Unblocked");
            self.stats.unblocked.fetch_add(1, Relaxed);
        }
        self.stats.add_trim_count(&trim_count);
    }

    pub fn unblock_account(&self, account: Account) {
        let unblocked;
        let trim_count;
        {
            let mut logic = self.logic.lock().unwrap();
            unblocked = logic.unblock(account);
            trim_count = logic.trim_overflow();
        }
        if unblocked {
            trace!(account = account.encode_account(), "Account unblocked");
            self.stats.unblocked.fetch_add(1, Relaxed);
        }
        self.stats.add_trim_count(&trim_count);
    }

    pub fn dependency_account_requested(&self, dependency: &BlockHash) {
        self.logic
            .lock()
            .unwrap()
            .dependency_account_requested(dependency);
        self.stats.dependency_requested.fetch_add(1, Relaxed);
        trace!(
            dependency = dependency.encode_hex(),
            "Dependency account requested"
        );
    }

    /// Removes the request mark for a dependency, so that the dependency
    /// can be requested again
    pub fn remove_dependency_request(&self, dependency: &BlockHash) {
        let removed = self
            .logic
            .lock()
            .unwrap()
            .remove_dependency_request(dependency);
        if removed {
            self.stats.dependency_request_removed.fetch_add(1, Relaxed);
            trace!(
                dependency = dependency.encode_hex(),
                "Dependency request removed"
            );
        }
    }

    pub fn dependency_account_not_found(&self, dependency: &BlockHash) {
        self.logic
            .lock()
            .unwrap()
            .dependency_account_not_found(dependency);

        trace!(
            dependency = dependency.encode_hex(),
            "Dependency account not found"
        );
    }

    /// Sets information about the account chain that contains the block hash
    pub fn dependency_update(&self, dependency: &BlockHash, dependency_account: Account) {
        let mut logic = self.logic.lock().unwrap();
        let (updated, dep_inserted) = logic.dependency_update(dependency, dependency_account);
        if updated > 0 {
            self.stats
                .dependency_update
                .fetch_add(updated as u64, Relaxed);
            if dep_inserted {
                self.stats.inserted.fetch_add(1, Relaxed);
            }
            trace!(
                dependency = dependency.encode_hex(),
                dependency_account = dependency_account.encode_account(),
                "Dependency updated"
            );
        }
    }

    pub fn next_download_target(&self) -> Option<DownloadTarget> {
        self.logic.lock().unwrap().next_download_target()
    }

    pub fn take_next_block_for_processing(&self) -> Option<Block> {
        let block = self.logic.lock().unwrap().take_next_block_for_processing();
        if block.is_some() {
            self.stats.processing_started.fetch_add(1, Relaxed);
        }
        block
    }

    pub fn revert_processing_started(&self, block_hash: &BlockHash) {
        let reverted = self
            .logic
            .lock()
            .unwrap()
            .revert_processing_started(block_hash);
        if reverted {
            self.stats.processing_started_reverted.fetch_add(1, Relaxed);
        }
    }

    pub fn download_started(&self, account: &Account) {
        let started = self.logic.lock().unwrap().download_started(account);
        if started {
            trace!(account = account.encode_account(), "Download started");
            self.stats.download_started.fetch_add(1, Relaxed);
        } else {
            self.stats.download_start_failed.fetch_add(1, Relaxed);
        }
    }

    /// Puts a downloading account back into the download queue, so that it
    /// can be requested again
    pub fn requeue_download(&self, account: &Account) {
        let requeued = self.logic.lock().unwrap().requeue_download(account);
        if requeued {
            trace!(account = account.encode_account(), "Download requeued");
            self.stats.download_requeued.fetch_add(1, Relaxed);
        }
    }

    /// If an outdated channel is detected its id is returned
    pub fn download_finished(
        &self,
        account: &Account,
        blocks: VecDeque<Block>,
        should_cool_down: bool,
        channel_id: ChannelId,
    ) -> Option<ChannelId> {
        let finished;
        let outdated_channel: Option<ChannelId>;
        let mut prio_up_result = None;
        let mut prio_down_result = None;
        let block_count = blocks.len();
        {
            let mut logic = self.logic.lock().unwrap();
            (finished, outdated_channel) =
                logic.download_finished(account, blocks, should_cool_down, channel_id);
            if finished {
                if should_cool_down {
                    prio_down_result = Some(logic.priority_down(account));
                } else if block_count > 0 {
                    prio_up_result = Some(logic.priority_up(account));
                }
            }
        }

        if finished {
            self.stats.download_finished.fetch_add(1, Relaxed);
            trace!(
                account = account.encode_account(),
                block_count, "Download finished"
            );
        } else {
            self.stats.download_finished_failed.fetch_add(1, Relaxed);
            trace!(
                account = account.encode_account(),
                block_count, "Finished download couldn't be processed!"
            );
        }

        if let Some(result) = prio_up_result {
            self.stats.add_prio_up_result(&result);
            trace!(
                ?result,
                account = account.encode_account(),
                "Priority increased"
            );
        }
        if let Some(result) = prio_down_result {
            self.stats.add_prio_down_result(&result);
            trace!(
                ?result,
                account = account.encode_account(),
                "Priority decreased"
            );
        }

        outdated_channel
    }

    pub fn processing_finished(&self, block_hash: &BlockHash) {
        let finished = self.logic.lock().unwrap().processing_finished(block_hash);
        if finished {
            self.stats.processing_finished.fetch_add(1, Relaxed);
            trace!(block = block_hash.encode_hex(), "Processing finished");
        } else {
            self.stats.processing_finished_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn processing_failed(&self, block_hash: &BlockHash) {
        self.logic.lock().unwrap().processing_failed(block_hash);
    }

    pub fn reprocess(&self, block_hash: &BlockHash) {
        let reprocessed = self.logic.lock().unwrap().reprocess(block_hash);
        if reprocessed {
            self.stats.reprocess.fetch_add(1, Relaxed);
        } else {
            self.stats.reprocess_failed.fetch_add(1, Relaxed);
        }
    }

    pub fn next_unknown_blocking_hash(&self) -> Option<BlockHash> {
        self.logic.lock().unwrap().next_unknown_blocking_hash()
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

    /// Should be called periodically to remove old entries from the blocked accounts
    pub fn decay(&self) {
        let now = self.clock.now();
        let decayed;
        let trim_count;
        {
            let mut logic = self.logic.lock().unwrap();
            decayed = logic.decay_blocked_accounts(now);
            trim_count = logic.trim_overflow();
        }
        self.stats
            .decayed_blocked
            .fetch_add(decayed as u64, Relaxed);
        self.stats.add_trim_count(&trim_count);
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
