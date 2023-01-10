use std::collections::VecDeque;
use std::sync::Arc;
use std::thread;
use std::thread::spawn;
use rsnano_core::{HashOrAccount, UncheckedInfo};
use rsnano_store_lmdb::LmdbStore;
use rsnano_store_traits::Store;

type Insert = (HashOrAccount, UncheckedInfo);
type Query = HashOrAccount;

pub enum Op {
    Insert(Insert),
    Query(Query),
}

pub struct UncheckedMap {
    store: Arc<LmdbStore>,
    disable_delete: bool,
    buffer: VecDeque<Op>,
    back_buffer: VecDeque<Op>,
}

impl UncheckedMap {
    pub fn new(store: Arc<LmdbStore>, disable_delete: bool) -> Self {
        Self {
            store,
            disable_delete,
            buffer: VecDeque::new(),
            back_buffer: VecDeque::new(),
        }
    }

    pub fn run(&self) {
        thread::spawn(|| {

        });
    }
}