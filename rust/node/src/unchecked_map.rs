use std::{sync::{Arc, Mutex, Condvar, MutexGuard}, thread::{JoinHandle, self}, collections::{VecDeque, BTreeMap, HashMap, BTreeSet}, mem::{self, size_of}, cell::RefCell, rc::Rc, cmp::Ordering};
use rsnano_core::{HashOrAccount, UncheckedInfo, UncheckedKey, BlockHash};
use rsnano_store_lmdb::LmdbStore;
use rsnano_store_traits::{WriteTransaction, Transaction, Store, UncheckedStore, UncheckedIterator};
use crate::stats::{Stats, StatType, DetailType, Direction};

const MEM_BLOCK_COUNT_MAX: usize = 64 * 1024;

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
    buffer: VecDeque<HashOrAccount>,
    back_buffer: VecDeque<HashOrAccount>,
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
                self.process_queries(back_buffer);
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

    fn process_queries(&self, back_buffer: &VecDeque<HashOrAccount>) {
        for item in back_buffer
        {
            self.query_impl (item);
        }
        
    }

    pub fn trigger(&self, dependency: HashOrAccount) {
        let mut lock = self.mutable.lock().unwrap();
        lock.buffer.push_back(dependency);
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

    pub fn entries_count(&self) -> usize {
        let lock = self.mutable.lock().unwrap();
        return lock.entries_container.size();
    }

    pub fn entries_size(&self) -> usize {
        let lock = self.mutable.lock().unwrap();
        std::mem::size_of_val(&lock.entries_container)
    }

    pub fn buffer_count(&self) -> usize {
        let lock = self.mutable.lock().unwrap();
        lock.buffer.len()
    }

    pub fn buffer_size(&self) -> usize {
        let lock = self.mutable.lock().unwrap();
        std::mem::size_of_val(&lock.buffer)
    }

    pub fn put(&self, dependency: HashOrAccount, info: UncheckedInfo) {
        println!("50");
        let mut lock = self.mutable.lock().unwrap();
        let key = UncheckedKey::new(dependency.into(), info.block.clone().unwrap().read().unwrap().hash());
        lock.entries_container.insert(Entry::new(key, info));
	    if lock.entries_container.size () > MEM_BLOCK_COUNT_MAX
	    {
		    lock.entries_container.pop_front ();
	    }
	    lock.stats.inc (StatType::Unchecked, DetailType::Put, Direction::In);
    }

    pub fn get(&self, transaction: &dyn Transaction, dependency: HashOrAccount) -> Vec<UncheckedInfo> {
        let mutex = Arc::new(Mutex::new(Vec::new()));
        let mutex_copy = Arc::clone(&mutex);
        let result = Arc::try_unwrap(mutex).unwrap().into_inner().unwrap();
        println!("result: {:?}", result);
        result
    }

    pub fn exists(&self, key: &UncheckedKey) -> bool {
        self.entries_count() != 0
    }

    pub fn del(&self, key: &UncheckedKey) {
        let mut lock = self.mutable.lock().unwrap();
        println!("31");
        let erase = lock.entries_container.by_key.remove(key);
        debug_assert!(erase.is_some());
    }

    pub fn clear(&self) {
        let mut lock = self.mutable.lock().unwrap();
        lock.entries_container.clear();
    }

    pub fn for_each1(&self, mut action: Box<dyn FnMut(&UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        println!("24");
        let lock = self.mutable.lock().unwrap();
        let entries = lock.entries_container.entries.clone();
        for entry in &entries {
            if predicate() {
                action(&entry.key, &entry.info);
            }
        }
    }

    pub fn for_each2(&self, dependency: &HashOrAccount, mut action: Box<dyn FnMut(&UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        println!("34");
        let lock = self.mutable.lock().unwrap();
        let entries = lock.entries_container.entries.clone();
        let key = UncheckedKey::new(dependency.into(), BlockHash::zero()); 
        let it = entries.iter().skip_while(|entry| entry.key != key);
        for entry in it {
            let block_hash: BlockHash = dependency.into();
            if predicate() && block_hash == entry.key.previous {
                println!("36");
                action(&entry.key, &entry.info);
            }
        }
    }

    pub fn query_impl(&self, hash: &HashOrAccount) {
        let mutex = Arc::clone(&self.mutable);
        let delete_queue = Arc::new(Mutex::new(VecDeque::new()));
        let delete_queue_copy = Arc::clone(&delete_queue); 
        self.for_each2(hash, Box::new(move |key, info| {
            let mut dq = delete_queue_copy.lock().unwrap();
            dq.push_back(key.clone());
            let lock = mutex.lock().unwrap();
            lock.stats.inc(StatType::Unchecked, DetailType::Satisfied, Direction::In);
            //satisfied.notify (info);
        }), Box::new(|| true));
        if !self.disable_delete {
            for key in &Arc::try_unwrap(delete_queue).unwrap().into_inner().unwrap() {
                self.del(key);
            }
        }
    }
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

        //self.entries.insert(entry);
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
        //self.entries.pop_first();
    }

    fn clear(&mut self) {
        self.entries = BTreeSet::new();
        self.by_key = HashMap::new();
        self.next_id = 0;
    }
}