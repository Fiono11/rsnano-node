use anyhow::Result;
use rsnano_core::utils::TomlWriter;
use serde::{Deserialize, Serialize};

#[derive(PartialEq, Eq, Clone, Copy, Debug, Deserialize, Serialize)]
pub enum SyncStrategy {
    /** Always flush to disk on commit. This is default. */
    Always,

    /** Do not flush meta data eagerly. This may cause loss of transactions, but maintains integrity. */
    NosyncSafe,

    /**
     * Let the OS decide when to flush to disk. On filesystems with write ordering, this has the same
     * guarantees as nosync_safe, otherwise corruption may occur on system crash.
     */
    NosyncUnsafe,
    /**
     * Use a writeable memory map. Let the OS decide when to flush to disk, and make the request asynchronous.
     * This may give better performance on systems where the database fits entirely in memory, otherwise is
     * may be slower.
     * @warning Do not use this option if external processes uses the database concurrently.
     */
    NosyncUnsafeLargeMemory,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LmdbConfig {
    pub sync: SyncStrategy,
    pub max_databases: u32,
    pub map_size: usize,
}

impl Default for LmdbConfig {
    fn default() -> Self {
        Self {
            sync: SyncStrategy::Always,
            max_databases: 128,
            map_size: 256 * 1024 * 1024 * 1024,
        }
    }
}

impl LmdbConfig {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn serialize_toml(&self, toml: &mut dyn TomlWriter) -> Result<()> {
        let sync_str = match self.sync {
            SyncStrategy::Always => "always",
            SyncStrategy::NosyncSafe => "nosync_safe",
            SyncStrategy::NosyncUnsafe => "nosync_unsafe",
            SyncStrategy::NosyncUnsafeLargeMemory => "nosync_unsafe_large_memory",
        };

        toml.put_str("sync", sync_str, "Sync strategy for flushing commits to the ledger database. This does not affect the wallet database.\ntype:string,{always, nosync_safe, nosync_unsafe, nosync_unsafe_large_memory}")?;
        toml.put_u32("max_databases", self.max_databases, "Maximum open lmdb databases. Increase default if more than 100 wallets is required.\nNote: external management is recommended when a large amounts of wallets are required (see https://docs.nano.org/integration-guides/key-management/).\ntype:uin32")?;
        toml.put_usize(
            "map_size",
            self.map_size,
            "Maximum ledger database map size in bytes.\ntype:uint64",
        )?;
        Ok(())
    }
}
