mod app;
mod ledger_adapter;
mod logic;

use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

use rsnano_ledger::{Ledger, ProcessResult};
use rsnano_types::{Account, BlockHash, SavedBlock};
use rsnano_utils::{
    container_info::{ContainerInfo, ContainerInfoProvider},
    stats::{StatsCollection, StatsSource},
};

use super::backlog_scan::UnconfirmedInfo;
pub use app::BoundedBacklog;
pub(crate) use ledger_adapter::BoundedBacklogLedgerAdapter;
pub use logic::BoundedBacklogConfig;

pub struct BoundedBacklogThread {
    thread: Mutex<Option<JoinHandle<()>>>,
    app: Arc<BoundedBacklog>,
}

impl BoundedBacklogThread {
    pub(crate) fn new(config: BoundedBacklogConfig, ledger: Arc<Ledger>) -> Self {
        Self {
            thread: Mutex::new(None),
            app: Arc::new(BoundedBacklog::new(config, ledger)),
        }
    }

    pub(crate) fn new2(app: Arc<BoundedBacklog>) -> Self {
        Self {
            thread: Mutex::new(None),
            app,
        }
    }

    pub fn new_null() -> Self {
        let config = BoundedBacklogConfig::default();
        let ledger = Arc::new(Ledger::new_null());
        Self::new(config, ledger)
    }

    pub fn start(&self) {
        debug_assert!(self.thread.lock().unwrap().is_none());

        let app = self.app.clone();
        let handle = std::thread::Builder::new()
            .name("Bounded backlog".to_owned())
            .spawn(move || app.run())
            .unwrap();

        *self.thread.lock().unwrap() = Some(handle);
    }

    pub fn stop(&self) {
        self.app.stop();

        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
    }

    pub fn set_cooldown(&self, cool_down: bool) {
        self.app.set_cooldown(cool_down);
    }

    /// Track unconfirmed blocks
    pub fn insert_processed(&self, batch: &[ProcessResult]) {
        self.app.insert_processed(batch);
    }

    pub fn erase_accounts(&self, accounts: &[Account]) {
        self.app.erase_accounts(accounts);
    }

    pub fn erase_hashes(&self, accounts: impl IntoIterator<Item = BlockHash>) {
        self.app.erase_hashes(accounts);
    }

    pub fn activate_batch(&self, batch: &[UnconfirmedInfo]) {
        self.app.activate_batch(batch);
    }

    pub fn remove(&self, confirmed: &Vec<(SavedBlock, BlockHash)>) {
        self.app.remove(confirmed);
    }
}

impl Drop for BoundedBacklogThread {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        debug_assert!(self.thread.lock().unwrap().is_none());
    }
}

impl StatsSource for BoundedBacklogThread {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.app.collect_stats(result);
    }
}

impl ContainerInfoProvider for BoundedBacklogThread {
    fn container_info(&self) -> ContainerInfo {
        self.app.container_info()
    }
}
