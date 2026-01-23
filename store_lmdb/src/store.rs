#[cfg(feature = "ledger_snapshots")]
use crate::forks_store::LmdbForksStore;
#[cfg(feature = "ledger_snapshots")]
use crate::snapshot_store::LmdbSnapshotStore;
use crate::{
    LmdbAccountStore, LmdbBlockStore, LmdbConfirmationHeightStore, LmdbFinalVoteStore,
    LmdbOnlineWeightStore, LmdbPeerStore, LmdbPendingStore, LmdbRepWeightStore, LmdbVersionStore,
    successor_store::LmdbSuccessorStore,
};
use rsnano_nullable_lmdb::{LmdbEnvironment, ReadTransaction, WriteTransaction};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub struct LedgerCache {
    pub confirmed_count: AtomicU64,
    pub block_count: AtomicU64,
    pub account_count: AtomicU64,
}

impl LedgerCache {
    pub fn new() -> Self {
        Self {
            confirmed_count: AtomicU64::new(0),
            block_count: AtomicU64::new(0),
            account_count: AtomicU64::new(0),
        }
    }

    pub fn reset(&self) {
        self.confirmed_count.store(0, Ordering::SeqCst);
        self.block_count.store(0, Ordering::SeqCst);
        self.account_count.store(0, Ordering::SeqCst);
    }
}

pub struct LmdbStore {
    pub env: LmdbEnvironment,
    pub cache: Arc<LedgerCache>,
    pub block: LmdbBlockStore,
    pub account: LmdbAccountStore,
    pub pending: LmdbPendingStore,
    pub rep_weight: Arc<LmdbRepWeightStore>,
    pub confirmation_height: LmdbConfirmationHeightStore,
    pub successors: LmdbSuccessorStore,
    // extract these?
    pub final_vote: LmdbFinalVoteStore,
    pub online_weight: LmdbOnlineWeightStore,
    pub peer: LmdbPeerStore,
    pub version: LmdbVersionStore,
    #[cfg(feature = "ledger_snapshots")]
    pub forks: LmdbForksStore,
    #[cfg(feature = "ledger_snapshots")]
    pub snapshots: LmdbSnapshotStore,
}

impl LmdbStore {
    pub fn new_null() -> Self {
        Self::new(LmdbEnvironment::new_null()).unwrap()
    }

    pub fn new(env: LmdbEnvironment) -> anyhow::Result<Self> {
        Ok(Self {
            cache: Arc::new(LedgerCache::new()),
            block: LmdbBlockStore::new(&env)?,
            account: LmdbAccountStore::new(&env)?,
            pending: LmdbPendingStore::new(&env)?,
            online_weight: LmdbOnlineWeightStore::new(&env)?,
            rep_weight: Arc::new(LmdbRepWeightStore::new(&env)?),
            peer: LmdbPeerStore::new(&env)?,
            confirmation_height: LmdbConfirmationHeightStore::new(&env)?,
            final_vote: LmdbFinalVoteStore::new(&env)?,
            successors: LmdbSuccessorStore::new(&env)?,
            version: LmdbVersionStore::new(&env)?,
            #[cfg(feature = "ledger_snapshots")]
            forks: LmdbForksStore::new(&env)?,
            #[cfg(feature = "ledger_snapshots")]
            snapshots: LmdbSnapshotStore::new(&env)?,
            env,
        })
    }

    pub fn memory_stats(&self) -> anyhow::Result<MemoryStats> {
        let stats = self.env.stat()?;
        Ok(MemoryStats {
            branch_pages: stats.branch_pages(),
            depth: stats.depth(),
            entries: stats.entries(),
            leaf_pages: stats.leaf_pages(),
            overflow_pages: stats.overflow_pages(),
            page_size: stats.page_size(),
        })
    }

    pub fn begin_read(&self) -> ReadTransaction {
        self.env.begin_read()
    }

    pub fn begin_write(&self) -> WriteTransaction {
        self.env.begin_write()
    }
}

#[derive(Serialize, Deserialize)]
pub struct MemoryStats {
    pub branch_pages: usize,
    pub depth: u32,
    pub entries: usize,
    pub leaf_pages: usize,
    pub overflow_pages: usize,
    pub page_size: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_lmdb::{EnvironmentFlags, EnvironmentOptions, LmdbEnvironmentFactory};

    #[test]
    fn create_store() -> anyhow::Result<()> {
        let options = EnvironmentOptions {
            max_dbs: 100,
            map_size: 1024,
            flags: EnvironmentFlags::empty(),
            path: "/nulled/store.ldb".into(),
        };
        let env = LmdbEnvironmentFactory::new_null().create(options)?;
        let _ = LmdbStore::new(env)?;
        Ok(())
    }
}
