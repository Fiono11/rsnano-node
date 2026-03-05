mod app;
mod ledger_adapter;
mod logic;

use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

pub use app::BoundedBacklog;
pub(crate) use ledger_adapter::BoundedBacklogLedgerAdapter;
pub use logic::BoundedBacklogConfig;

pub struct BoundedBacklogThread {
    thread: Mutex<Option<JoinHandle<()>>>,
    app: Arc<BoundedBacklog>,
}

impl BoundedBacklogThread {
    pub(crate) fn new(app: Arc<BoundedBacklog>) -> Self {
        Self {
            thread: Mutex::new(None),
            app,
        }
    }

    pub fn new_null() -> Self {
        Self::new(BoundedBacklog::new_null().into())
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
}

impl Drop for BoundedBacklogThread {
    fn drop(&mut self) {
        // Thread must be stopped before destruction
        debug_assert!(self.thread.lock().unwrap().is_none());
    }
}
