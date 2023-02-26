use std::{sync::{Arc, Mutex, Condvar, MutexGuard}, thread::{JoinHandle, self}, collections::{VecDeque, BTreeMap, HashMap, BTreeSet}, mem, cell::RefCell, rc::Rc, cmp::Ordering};
use rsnano_core::{HashOrAccount, UncheckedInfo, UncheckedKey, BlockHash};
use rsnano_store_lmdb::LmdbStore;
use rsnano_store_traits::{WriteTransaction, Transaction, Store, UncheckedStore, UncheckedIterator};
use crate::stats::{Stats, StatType, DetailType, Direction};

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
        let mut lock = self.thread.mutable.lock().unwrap();
        if !lock.stopped {
            lock.stopped = true;
            self.thread.condition.notify_all();
        }
        drop(lock);
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

    pub fn stats(mut self, stats: Arc<Stats>) -> Self {
        self.stats = Some(stats);
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
    stats: Arc<Stats>,
    satisfied: Box<dyn Fn(&UncheckedInfo) + Send + Sync>,
    counter: u8,
}

impl ThreadMutableData {
    fn new(stats: Arc<Stats>) -> Self {
        Self {
            stopped: false,
            buffer: VecDeque::new(),
            back_buffer: VecDeque::new(),
            writing_back_buffer: false,
            entries_container: EntriesContainer::new(),
            stats,
            satisfied: Box::new(move |_info: &UncheckedInfo| {}),
            counter: 0,
        }
    }
}

pub struct UncheckedMapThread {
    store: Arc<LmdbStore>,
    disable_delete: bool,
    mutable: Arc<Mutex<ThreadMutableData>>,
    condition: Arc<Condvar>,
    use_memory: Box<dyn Fn() -> bool + Send + Sync>,
}

impl UncheckedMapThread {
    fn new(store: Arc<LmdbStore>, stats: Arc<Stats>, disable_delete: bool) -> Self {
        Self {
            store,
            disable_delete,
            mutable: Arc::new(Mutex::new(ThreadMutableData::new(stats))),
            condition: Arc::new(Condvar::new()),
            use_memory: Box::new(move || { true }),
        }
    }

    fn run(&self) {
        let mut lock = self.mutable.lock().unwrap();
        while !lock.stopped {
            if !lock.buffer.is_empty() {
                println!("1");
                let temp = lock.buffer.clone();
                lock.buffer = lock.back_buffer.clone();
                lock.back_buffer = temp;
			    lock.writing_back_buffer = true;
                let back_buffer = &lock.back_buffer.clone();
                drop(lock);
                println!("2");
                self.write_buffer(back_buffer);
                println!("3");
                lock = self.mutable.lock().unwrap();
                lock.writing_back_buffer = false;
			    lock.back_buffer.clear ();
            }
            else {
                println!("4");
                self.condition.notify_all();
                println!("5");
                lock = self
                    .condition
                    .wait_while(lock, |other_lock| !other_lock.stopped && other_lock.buffer.is_empty())
                    .unwrap();
                println!("6");
            }
        }
    }

    fn write_buffer(&self, back_buffer: &VecDeque<Op>) {
        let mut transaction = self.store.tx_begin_write().unwrap();
        for item in back_buffer {
            match item {
                Op::Insert(i) => {
                    self.insert_impl(&mut transaction, i.0.clone(), i.1.clone());
                },
                Op::Query(q) => {
                    self.query_impl(&mut transaction, q.clone());
                },
            }
        }
    }

    pub fn trigger(&self, dependency: HashOrAccount) {
        let mut lock = self.mutable.lock().unwrap();
        lock.buffer.push_back(Op::Query(dependency));
        //debug_assert (buffer.back ().which () == 1);
        lock.stats.inc(StatType::Unchecked, DetailType::Trigger, Direction::In);
        drop(lock);
        self.condition.notify_all(); // Notify run ()
    }

    pub fn flush(&self) {
        let mut lock = self.mutable.lock().unwrap();
        println!("7");
        lock = self.condition.wait_while(lock, |other_lock| !other_lock.stopped && (!other_lock.buffer.is_empty() ||
        !other_lock.back_buffer.is_empty() || other_lock.writing_back_buffer)).unwrap();
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

    pub fn put(&self, dependency: HashOrAccount, info: UncheckedInfo) {
        println!("50");
        let mut lock = self.mutable.lock().unwrap();
        lock.buffer.push_back(Op::Insert((dependency, info)));
        lock.stats.inc(StatType::Unchecked, DetailType::Put, Direction::In);
        drop(lock);
        self.condition.notify_all();
    }

    pub fn get(&self, transaction: &dyn Transaction, dependency: HashOrAccount) -> Vec<UncheckedInfo> {
        let mutex = Arc::new(Mutex::new(Vec::new()));
        let mutex_copy = Arc::clone(&mutex);
        self.for_each2(transaction, dependency, Box::new(move |key, info| {
            let mut lock = mutex_copy.lock().unwrap();
            lock.push(info.clone());
        }), Box::new(|| true));
        let result = Arc::try_unwrap(mutex).unwrap().into_inner().unwrap();
        println!("result: {:?}", result);
        result
    }

    pub fn exists(&self, transaction: &dyn Transaction, key: &UncheckedKey) -> bool {
        let mut result = false;
        let lock = self.mutable.lock().unwrap();
        if lock.entries_container.is_empty() {
            result = self.store.unchecked().exists(transaction, key)
        } else {
            if let Some(_) = lock.entries_container.by_key.get(key) {
                result = true;
            } else {
                result = false
            }
        }
        println!("result1: {:?}", result);
        result
    }

    pub fn del(&self, transaction: &mut dyn WriteTransaction, key: &UncheckedKey) {
        let mut lock = self.mutable.lock().unwrap();
        if lock.entries_container.is_empty() {
            println!("30");
            self.store.unchecked_store.del(transaction, key);
        }
        else {
            println!("31");
            let erase = lock.entries_container.by_key.remove(key);
            debug_assert!(erase.is_some());
        }
    }

    pub fn clear(&self, transaction: &mut dyn WriteTransaction) {
        let mut lock = self.mutable.lock().unwrap();
        if lock.entries_container.is_empty() {
            self.store.unchecked_store.clear(transaction);
        }
        else {
            lock.entries_container.clear();
        }
    }

    pub fn for_each1(&self, transaction: &dyn Transaction, mut action: Box<dyn FnMut(&UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        println!("24");
        let lock = self.mutable.lock().unwrap();
        let empty = lock.entries_container.is_empty();
        let entries = lock.entries_container.entries.clone();
        drop(lock);
        if empty {
            println!("25");
            let mut it: UncheckedIterator = self.store.unchecked().begin(transaction);
            println!("42");
            while !it.is_end() {//&& predicate() {
                println!("8");
                let (uk, ui) = it.current().unwrap();
                action(uk, ui);
                it.next();
            }
        }
        else {
            println!("26");
            for entry in &entries { // predicate
                if predicate() {
                    action(&entry.key, &entry.info);
                }
            }
        }
    }

    pub fn for_each2(&self, transaction: &dyn Transaction, dependency: HashOrAccount, mut action: Box<dyn FnMut(&UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        println!("34");
        let lock = self.mutable.lock().unwrap();
        let empty = lock.entries_container.is_empty();
        let entries = lock.entries_container.entries.clone();
        drop(lock);
        let key = UncheckedKey::new(dependency.into(), BlockHash::zero()); 
        if empty {
            let mut it: UncheckedIterator = self.store.unchecked().lower_bound(transaction, &key);
            //let mut it: UncheckedIterator = self.store.unchecked().begin(transaction);
            //let mut it: UncheckedIterator = self.store.unchecked().equal_range(transaction, dependency.into());
            while !it.is_end() {//&& predicate() {
                println!("9");
                let (uk, ui) = it.current().unwrap();
                action(uk, ui);
                it.next();
            }
        }
        else {
            let it = entries.iter().skip_while(|entry| entry.key != key);
            for entry in it {
                let block_hash: BlockHash = dependency.into();
                if predicate() && block_hash == entry.key.previous {
                    println!("36");
                    action(&entry.key, &entry.info);
                }
            }
        }
    }

    fn insert_impl(&self, transaction: &mut dyn WriteTransaction, dependency: HashOrAccount, info: UncheckedInfo) {
        let mut lock = self.mutable.lock().unwrap();
        println!("11");
        // Check if block dependency has been satisfied while waiting to be placed in the unchecked map
        if self.store.block().exists(transaction.txn(), &BlockHash::from_bytes(*dependency.as_bytes()))
        {
            lock.satisfied.call((&info,));
            println!("12");
            return;
        }
        let empty = lock.entries_container.is_empty();
        drop(lock);
        if empty && self.use_memory.call(()) {
            println!("13");
            let entries_new = Arc::new(Mutex::new(EntriesContainer::new()));
            let entries_copy = Arc::clone(&entries_new);
            let entries_copy2 = Arc::clone(&entries_new);
            self.for_each1(transaction.txn(), Box::new(move |key, info| { 
                println!("15");
                let mut lock = entries_copy.lock().unwrap();
                lock.insert(Entry::new(key.clone(), info.clone()));
                drop(lock);
                }), 
            Box::new(move || {
                println!("16");
                let lock = entries_copy2.lock().unwrap();
                let bool = entries_copy2.lock().unwrap().size() < MEM_BLOCK_COUNT_MAX;
                drop(lock);
                bool
            }));
            println!("17");
            self.clear(transaction);
            lock = self.mutable.lock().unwrap();
            lock.entries_container = Arc::try_unwrap(entries_new).unwrap().into_inner().unwrap();
            drop(lock);
        }
        lock = self.mutable.lock().unwrap();
        if lock.entries_container.is_empty() {
            println!("18");
            self.store.unchecked().put(transaction, &dependency, &info);
        }
        else {
            println!("19");
            let key = UncheckedKey::new(dependency.into(), info.block.as_ref().unwrap().clone().read().unwrap().as_block().hash());
            let entry = Entry {
                key,
                info
            };
            lock.entries_container.insert(entry);
            while lock.entries_container.size() > MEM_BLOCK_COUNT_MAX
            {
                println!("10");
                lock.entries_container.pop_front();
            }
        }
    }

    fn query_impl(&self, transaction: &mut dyn WriteTransaction, hash: HashOrAccount) {
        println!("20");
        let mutex = Arc::clone(&self.mutable);
        let delete_queue = Arc::new(Mutex::new(VecDeque::new()));
        let delete_queue_copy = Arc::clone(&delete_queue); 
        self.for_each2(transaction.txn(), hash, Box::new(move |key, info| {
            println!("21");
            println!("41");
            let mut dq = delete_queue_copy.lock().unwrap();
            dq.push_back(key.clone());
            drop(dq);
            let lock = mutex.lock().unwrap();
            lock.stats.inc(StatType::Unchecked, DetailType::Satisfied, crate::stats::Direction::In); // ir or out
		    lock.satisfied.call((info,));
            drop(lock);
            println!("40");
        }), Box::new(|| true));
        if !self.disable_delete {
            println!("22");
            for key in &Arc::try_unwrap(delete_queue).unwrap().into_inner().unwrap() {
                println!("23");
                self.del(transaction, key);
            }
        }
    }
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

impl Entry {
    fn new(key: UncheckedKey, info: UncheckedInfo) -> Self {
        Self {
            key,
            info,
        }
    }
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.key.eq(&other.key)
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.key.partial_cmp(&other.key)
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

#[derive(Default, Clone, Debug)]
pub struct EntriesContainer {
    entries: BTreeSet<Entry>, 
    by_key: HashMap<UncheckedKey, usize>,
    next_id: usize,
}

impl EntriesContainer {
    fn new() -> Self {
        Self {
            entries: BTreeSet::new(),
            by_key: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn insert(&mut self, entry: Entry) {
        let id = self.create_id();

        self.by_key.insert(entry.clone().key, id);

        self.entries.insert(entry);
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
        self.entries = BTreeSet::new();
        self.by_key = HashMap::new();
        self.next_id = 0;
    }
}