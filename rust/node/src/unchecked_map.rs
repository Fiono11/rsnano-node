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
    join_handle: Option<JoinHandle<()>>,
    pub thread: Arc<UncheckedMapThread>,
}

impl UncheckedMap {
    pub fn builder() -> Builder {
        Builder::new()
    }

    pub fn stop(&mut self) {
        let mut mutex = self.thread.mutable.lock().unwrap();
        if !mutex.stopped {
            mutex.stopped = true;
            self.thread.condition.notify_all();
        }
        if let Some(handle) = self.join_handle.take() {
            handle.join().unwrap();
        }
    }
}

#[derive(Default)]
pub struct Builder {
    store: Option<Arc<LmdbStore>>,
    disable_delete: bool,
    stats: Option<Arc<Stats>>,
}

impl Builder {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn store(mut self, store: Arc<LmdbStore>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn disable_delete(mut self, disable_delete: bool) -> Self {
        self.disable_delete = disable_delete;
        self
    }

    pub fn spawn(self) -> std::io::Result<UncheckedMap> {
        let thread = Arc::new(UncheckedMapThread::new(self.store.unwrap(), self.stats.unwrap(), self.disable_delete));

        let thread_clone = thread.clone();
        let join_handle = std::thread::Builder::new()
            .name("Unchecked".to_string())
            .spawn(move || {
                thread_clone.run();
            })?;

        Ok(UncheckedMap {
            join_handle: Some(join_handle),
            thread,
        })
    }
}

struct ThreadMutableData {
    stopped: bool,
    buffer: VecDeque<Op>,
    back_buffer: VecDeque<Op>,
    writing_back_buffer: bool,
    entries_container: EntriesContainer,
}

impl ThreadMutableData {
    fn new() -> Self {
        Self {
            stopped: false,
            buffer: VecDeque::new(),
            back_buffer: VecDeque::new(),
            writing_back_buffer: false,
            entries_container: EntriesContainer::new(),
        }
    }
}

pub struct UncheckedMapThread {
    store: Arc<LmdbStore>,
    stats: Arc<Stats>,
    disable_delete: bool,
    mutable: Mutex<ThreadMutableData>,
    condition: Condvar,
}

impl UncheckedMapThread {
    fn new(store: Arc<LmdbStore>, stats: Arc<Stats>, disable_delete: bool) -> Self {
        Self {
            store,
            stats,
            disable_delete,
            mutable: Mutex::new(ThreadMutableData::new()),
            condition: Condvar::new(),
        }
    }

    fn run(&self) {
        let mut lock = self.mutable.lock().unwrap();
        while !lock.stopped {
            if !lock.buffer.is_empty() {
                let temp = lock.buffer.clone();
                lock.buffer = lock.back_buffer.clone();
                lock.back_buffer = temp;
			    lock.writing_back_buffer = true;
                let back_buffer = &lock.back_buffer.clone();
                drop(lock);
                self.write_buffer(back_buffer);
                lock = self.mutable.lock().unwrap();
                lock.writing_back_buffer = false;
			    lock.back_buffer.clear ();
            }
            else {
                lock = self
                    .condition
                    .wait_while(lock, |other_lock| !other_lock.stopped && other_lock.buffer.is_empty())
                    .unwrap();
            }
        }
    }

    fn write_buffer(&self, back_buffer: &VecDeque<Op>) {
        let mut transaction = self.store.tx_begin_write().unwrap();
        for item in back_buffer {
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
        let mut lock = self.mutable.lock().unwrap();
        lock.buffer.push_back(Op::Query(dependency));
        self.condition.notify_all(); // Notify run ()
    }

    pub fn flush(&self) {
        let mut lock = self.mutable.lock().unwrap();
        lock = self.condition.wait_while(lock, |other_lock| !other_lock.stopped && (!other_lock.buffer.is_empty() ||
        !other_lock.back_buffer.is_empty() || !other_lock.writing_back_buffer)).unwrap();
    }

    pub fn count(&self, transaction: &dyn Transaction) -> usize {
        let mut lock = self.mutable.lock().unwrap();
        if lock.entries_container.is_empty() {
            return self.store.unchecked_store.count(transaction) as usize;
        }
        else {
            return lock.entries_container.size();
        }
    }

    pub fn put(&mut self, dependency: HashOrAccount, info: UncheckedInfo) {
        let mut lock = self.mutable.lock().unwrap();
        lock.buffer.push_back(Op::Insert((dependency, info)));
        self.condition.notify_all();
    }

    pub fn get(&self, transaction: &dyn Transaction, dependency: BlockHash) -> Vec<UncheckedInfo> {
        let mut lock = self.mutable.lock().unwrap();
        let mut result = Vec::new();
        if lock.entries_container.is_empty()
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
            for (_, entry) in lock.entries_container.entries.iter() { // predicate
                if entry.key.previous == dependency {
                    //action(&entry.key, &entry.info);
                    result.push(entry.info.clone());
                }
            }
        }
        result
    }

    pub fn exists(&self, transaction: &dyn Transaction, key: &UncheckedKey) -> bool {
        let mut lock = self.mutable.lock().unwrap();
        return if lock.entries_container.is_empty() {
            self.store.unchecked().exists(transaction, key)
        } else {
            if let Some(_) = lock.entries_container.by_key.get(key) {
                true
            } else {
                false
            }
        }
    }

    pub fn del(&mut self, transaction: &mut dyn WriteTransaction, key: &UncheckedKey) {
        let mut lock = self.mutable.lock().unwrap();
        if lock.entries_container.is_empty() {
            self.store.unchecked_store.del(transaction, key);
        }
        else {
            let erase = lock.entries_container.by_key.remove(key);
            debug_assert!(erase.is_some());
        }
    }

    pub fn clear(&mut self, transaction: &mut dyn WriteTransaction) {
        let mut lock = self.mutable.lock().unwrap();
        if lock.entries_container.is_empty() {
            self.store.unchecked_store.clear(transaction);
        }
        else {
            lock.entries_container.clear();
        }
    }

    pub fn for_each1(&self, transaction: &dyn Transaction, mut action: Box<dyn FnMut(&mut EntriesContainer, &UncheckedKey, &UncheckedInfo)>) {
        let mut lock = self.mutable.lock().unwrap();
        if lock.entries_container.is_empty() {
            let mut it = self.store.unchecked().begin(transaction);
            while !it.is_end() {
                if lock.entries_container.size() < MEM_BLOCK_COUNT_MAX { // predicate
                    let (key, info) = it.current().unwrap();
                    action(&mut lock.entries_container, key, info);
                    let entry = Entry {
                        key: key.clone(),
                        info: info.clone(),
                    };
                    lock.entries_container.insert(entry);
                }
            }
        }
        else {
            let mut entries = lock.entries_container.clone();
            for (_, entry) in lock.entries_container.entries.iter() { // predicate
                if lock.entries_container.entries.len() < MEM_BLOCK_COUNT_MAX {
                    action(&mut entries, &entry.key, &entry.info);
                }
            }
        }
    }

    pub fn for_each2(&self, transaction: &dyn Transaction, dependency: BlockHash, mut action: Box<dyn FnMut(&mut EntriesContainer, &UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        let mut lock = self.mutable.lock().unwrap();
        //let dependency = BlockHash::from_bytes(*dependency.as_bytes());
        if lock.entries_container.is_empty()
        {
            let (mut i, n) = self.store.unchecked_store.equal_range(transaction, dependency);
            while !i.is_end() {
                if predicate() && i.current().unwrap().0.hash == dependency {
                    println!("F");
                    let (key, info) = i.current().unwrap();
                    action(&mut lock.entries_container, key, info);
                }
                i.next();
            }
        }
        else
        {
            let mut entries = lock.entries_container.clone();
            for (_, entry) in entries.entries.iter() { // predicate
                println!("G");
                if predicate() && entry.key.previous == dependency {
                    println!("H");
                    action(&mut lock.entries_container, &entry.key, &entry.info);
                }
            }
        }
    }

    /*fn insert_impl(&self, transaction: &mut dyn WriteTransaction, dependency: HashOrAccount, info: UncheckedInfo) {
        if self.entries_container.is_empty() {//&& (self.use_memory)() {
            let mut entries_new = EntriesContainer::new();
            //let a = |entries_new: &mut EntryContainer, key: &UncheckedKey, info: &UncheckedInfo| {
                //entries_new.insert(Entry { key: key.clone(), info: info.clone()});
            //};
            //self.for_each2(transaction.txn(), BlockHash::from_bytes(*dependency.as_bytes()), Box::new(a), Box::new(|| true));

            if self.entries_container.is_empty()
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

#[derive(Default, Clone, Debug)]
pub struct EntriesContainer {
    entries: BTreeMap<usize, Entry>, //BTreeSet?
    by_key: HashMap<UncheckedKey, usize>,
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