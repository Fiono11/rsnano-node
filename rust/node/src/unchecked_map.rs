use std::{sync::{Arc, Mutex, Condvar, MutexGuard}, thread::{JoinHandle, self}, collections::{VecDeque, BTreeMap, HashMap}, mem};
use rsnano_core::{HashOrAccount, UncheckedInfo, UncheckedKey, BlockHash};
use rsnano_store_lmdb::LmdbStore;
use rsnano_store_traits::{WriteTransaction, Transaction, Store, UncheckedStore};
use crate::stats::Stats;

const MEM_BLOCK_COUNT_MAX: usize = 256000;

struct UncheckedMapFlags {
    stopped: bool,
    active: bool,
}

pub struct UncheckedMap {
    store: Arc<LmdbStore>,
    stats: Arc<Stats>,
    disable_delete: bool,
    stopped: bool,
    active: bool,
    buffer: VecDeque<Op>,
    back_buffer: VecDeque<Op>,
    writing_back_buffer: bool,
    entries: EntriesContainer,
    condition: Arc<Condvar>,
    mutex: Arc<Mutex<UncheckedMapFlags>>,
    thread: Option<JoinHandle<()>>,
}

impl UncheckedMap {
    pub fn new(store: Arc<LmdbStore>, stats: Arc<Stats>, disable_delete: bool) -> Self {
        Self {
            store,
            stats,
            disable_delete,
            stopped: false,
            active: false,
            buffer: VecDeque::new(),
            back_buffer: VecDeque::new(),
            writing_back_buffer: false,
            entries: EntriesContainer::new(),
            condition: Arc::new(Condvar::new()),
            mutex: Arc::new(Mutex::new(UncheckedMapFlags {
                stopped: false,
                active: false,
            })),
            thread: None,
        }
    }

    pub fn start(&mut self) {
        debug_assert!(self.thread.is_none());

        let mut thread = UncheckedMapThread {
            store: Arc::clone(&self.store),
            stats: Arc::clone(&self.stats),
            disable_delete: self.disable_delete,
            buffer: VecDeque::new(),
            back_buffer: VecDeque::new(),
            writing_back_buffer: false,
            entries_container: EntriesContainer::new(),
            condition: Arc::new(Condvar::new()),
            mutex: Arc::clone(&self.mutex),
        };

        self.thread = Some(
            thread::Builder::new()
                .name("Unchecked".to_owned())
                .spawn(move || {
                    thread.run();
                })
                .unwrap(),
        );
    }

    pub fn stop(&mut self) {
        let mut thread = self.mutex.lock().unwrap();
        if !thread.stopped {
            thread.stopped = true;
            self.condition.notify_all();
        }
        if let Some(handle) = self.thread.take() {
            handle.join().unwrap();
        }
    }
}

struct UncheckedMapThread {
    store: Arc<LmdbStore>,
    stats: Arc<Stats>,
    disable_delete: bool,
    buffer: VecDeque<Op>,
    back_buffer: VecDeque<Op>,
    writing_back_buffer: bool,
    entries_container: EntriesContainer,
    condition: Arc<Condvar>,
    mutex: Arc<Mutex<UncheckedMapFlags>>,
}

impl UncheckedMapThread {
    fn run(&mut self) {
        let mut lock = self.mutex.lock().unwrap();
        while !lock.stopped {
            if !self.buffer.is_empty() {
                mem::swap(&mut self.buffer, &mut self.back_buffer);
			    self.writing_back_buffer = true;
                lock.active = false;
                drop(lock);
                self.write_buffer();
                lock = self.mutex.lock().unwrap();
                self.writing_back_buffer = false;
			    self.back_buffer.clear ();
            }
            else {
                lock = self
                    .condition
                    .wait_while(lock, |thread| !thread.stopped && !self.buffer.is_empty())
                    .unwrap();
            }
        }
    }

    fn write_buffer(&self) {
        let mut transaction = self.store.tx_begin_write().unwrap();
        for item in &self.back_buffer {
            match item {
                Op::Insert(i) => {
                    //self.insert_impl(data, &mut transaction, i.0.clone(), i.1.clone());
                },
                Op::Query(q) => {
                    //self.query_impl(data, &mut transaction, BlockHash::from(q.number()));
                },
            }
        }
    }

    pub fn trigger(&mut self, dependency: HashOrAccount) {
        self.buffer.push_back(Op::Query(dependency));
        self.condition.notify_all(); // Notify run ()
    }

    pub fn flush(&self) {
        let mut lock = self.mutex.lock().unwrap();
        lock = self.condition.wait_while(lock, |thread| !thread.stopped && (!self.buffer.is_empty() ||
        !self.back_buffer.is_empty() || !self.writing_back_buffer)).unwrap();
    }

    pub fn count(&self, transaction: &dyn Transaction) -> usize {
        if self.entries_container.is_empty() {
            return self.store.unchecked_store.count(transaction) as usize;
        }
        else {
            return self.entries_container.size();
        }
    }

    pub fn put(&mut self, dependency: HashOrAccount, info: UncheckedInfo) {
        self.buffer.push_back(Op::Insert((dependency, info)));
        self.condition.notify_all();
    }

    pub fn get(&self, transaction: &dyn Transaction, dependency: BlockHash) -> Vec<UncheckedInfo> {
        let mut result = Vec::new();
        if self.entries_container.is_empty()
        {
            let (mut i, n) = self.store.unchecked_store.equal_range(transaction, dependency);
            while !i.is_end() {
                if i.current().unwrap().0.hash == dependency {
                    let (key, info) = i.current().unwrap();
                    //action(key, info);
                    result.push(info.clone());
                }
                i.next();
            }
        }
        else
        {
            for (_, entry) in self.entries_container.entries.iter() { // predicate
                if entry.key.previous == dependency {
                    //action(&entry.key, &entry.info);
                    result.push(entry.info.clone());
                }
            }
        }
        result
    }

    pub fn exists(&self, transaction: &dyn Transaction, key: &UncheckedKey) -> bool {
        return if self.entries_container.is_empty() {
            self.store.unchecked().exists(transaction, key)
        } else {
            if let Some(_) = self.entries_container.by_key.get(key) {
                true
            } else {
                false
            }
        }
    }

    pub fn del(&mut self, transaction: &mut dyn WriteTransaction, key: &UncheckedKey) {
        if self.entries_container.is_empty() {
            self.store.unchecked_store.del(transaction, key);
        }
        else {
            let erase = self.entries_container.by_key.remove(key);
            debug_assert!(erase.is_some());
        }
    }

    pub fn clear(&mut self, transaction: &mut dyn WriteTransaction) {
        if self.entries_container.is_empty() {
            self.store.unchecked_store.clear(transaction);
        }
        else {
            self.entries_container.clear();
        }
    }

    /*fn insert_impl(&self, transaction: &mut dyn WriteTransaction, dependency: HashOrAccount, info: UncheckedInfo) {
        if self.entries.is_empty() {//&& (self.use_memory)() {
            let mut entries_new = EntryContainer::new();
            //let a = |entries_new: &mut EntryContainer, key: &UncheckedKey, info: &UncheckedInfo| {
                //entries_new.insert(Entry { key: key.clone(), info: info.clone()});
            //};
            //self.for_each2(transaction.txn(), BlockHash::from_bytes(*dependency.as_bytes()), Box::new(a), Box::new(|| true));

            if self.entries.is_empty()
            {
                
            }
            else
            {
                let entries = lock.entries.clone();
                for (_, entry) in entries.entries.iter() { // predicate
                    if predicate() && entry.key.previous == dependency {
                        action(&mut lock.entries, &entry.key, &entry.info);
                    }
                }
            }

            self.clear(transaction);
		    data.entries = entries_new;
        }
        if data.entries.is_empty() {
            self.store.unchecked().put(transaction, &dependency, &info);
        }
        else {
            let key = UncheckedKey::new(info.previous(), info.hash());
            let entry = Entry {
                key,
                info
            };
            data.entries.insert(entry);
            while data.entries.size() > MEM_BLOCK_COUNT_MAX
            {
                data.entries.pop_front();
            }
        }
    }*/
}

#[derive(Clone)]
enum Op {
    Insert((HashOrAccount, UncheckedInfo)),
    Query(HashOrAccount),
}

#[derive(Clone, Debug)]
pub struct Entry {
    key: UncheckedKey,
    info: UncheckedInfo,
}

//#[derive(Default, Clone, Debug)]
pub struct EntriesContainer {
    entries: BTreeMap<usize, Entry>, //BTreeSet?
    by_key: HashMap<UncheckedKey, usize>,
    //by_info:
    next_id: usize,
}

impl EntriesContainer {
    fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            by_key: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn insert(&mut self, entry: Entry) {
        let id = self.create_id();

        self.by_key.insert(entry.clone().key, id);

        self.entries.insert(id, entry);
    }

    fn create_id(&mut self) -> usize {
        let mut id = self.next_id;
        id += 1;
        id
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn size(&self) -> usize {
        self.entries.len()
    }

    fn pop_front(&mut self) {
        self.entries.pop_first();
    }

    fn clear(&mut self) {
        self.entries = BTreeMap::new();
        self.by_key = HashMap::new();
        self.next_id = 0;
    }
}