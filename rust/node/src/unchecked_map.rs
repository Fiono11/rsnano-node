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

    pub fn exists(&self, key: &UncheckedKey) -> bool {
        self.thread.exists(key)
    }

    pub fn put(&self, dependency: HashOrAccount, info: UncheckedInfo) {
        self.thread.stats.inc (StatType::Unchecked, DetailType::Put, Direction::In);
        self.thread.put(dependency, info)
    }

    pub fn get(&self, hash: &HashOrAccount) -> Vec<UncheckedInfo> {
        self.thread.get(hash)
    }

    pub fn clear(&self) {
        self.thread.clear()
    }

    pub fn trigger(&self, dependency: &HashOrAccount) {
        self.thread.stats.inc(StatType::Unchecked, DetailType::Trigger, Direction::In);
        self.thread.trigger(dependency)
    }

    pub fn flush(&self) {
        self.thread.flush()
    }

    pub fn del(&self, key: &UncheckedKey) {
        self.thread.del(key) 
    }

    pub fn entries_count(&self) -> usize {
        self.thread.entries_count()
    }

    pub fn entries_size(&self) -> usize {
        self.thread.entries_size()
    }

    pub fn buffer_count(&self) -> usize {
        self.thread.buffer_count()
    }

    pub fn buffer_size(&self) -> usize {
        self.thread.buffer_size()
    }

    pub fn for_each1(&self, mut action: Box<dyn FnMut(&UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        self.thread.for_each1(action, predicate)
    }

    pub fn for_each2(&self, dependency: &HashOrAccount, mut action: Box<dyn FnMut(&UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        self.thread.for_each2(dependency, action, predicate)
    }
}

impl Drop for UncheckedMap {
    fn drop(&mut self) {
        self.stop()
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
    satisfied: Box<dyn Fn(&UncheckedInfo) + Send + Sync>,
    counter: u8,
}

impl ThreadMutableData {
    fn new() -> Self {
        Self {
            stopped: false,
            buffer: VecDeque::new(),
            back_buffer: VecDeque::new(),
            writing_back_buffer: false,
            entries_container: EntriesContainer::new(),
            satisfied: Box::new(move |_info: &UncheckedInfo| {}),
            counter: 0,
        }
    }
}

pub struct UncheckedMapThread {
    store: Arc<LmdbStore>,
    disable_delete: bool,
    stats: Arc<Stats>,
    mutable: Arc<Mutex<ThreadMutableData>>,
    condition: Arc<Condvar>,
    use_memory: Box<dyn Fn() -> bool + Send + Sync>,
}

impl UncheckedMapThread {
    fn new(store: Arc<LmdbStore>, stats: Arc<Stats>, disable_delete: bool) -> Self {
        Self {
            store,
            disable_delete,
            stats,
            mutable: Arc::new(Mutex::new(ThreadMutableData::new())),
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

    pub fn trigger(&self, dependency: &HashOrAccount) {
        println!("60");
        let mut lock = self.mutable.lock().unwrap();
        lock.buffer.push_back(dependency.clone());
        //debug_assert (buffer.back ().which () == 1);
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
    }

    pub fn get(&self, dependency: &HashOrAccount) -> Vec<UncheckedInfo> {
        let mutex = Arc::new(Mutex::new(Vec::new()));
        let mutex_copy = Arc::clone(&mutex);
        self.for_each2(&dependency, Box::new(move |key, info| {
            let mut lock = mutex_copy.lock().unwrap();
            lock.push(info.clone());
        }), Box::new(|| true));
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
        println!("25");
        let entries = lock.entries_container.by_id.clone();
        //drop(lock);
        for (id, entry) in &entries {
            println!("26");
            if predicate() {
                println!("27");
                action(&entry.key, &entry.info);
            }
        }
        println!("28");
    }

    pub fn for_each2(&self, dependency: &HashOrAccount, mut action: Box<dyn FnMut(&UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        println!("34");
        let lock = self.mutable.lock().unwrap();
        let by_key = lock.entries_container.by_key.clone();
        let by_id = lock.entries_container.by_id.clone();
        //drop(lock);
        println!("35");
        let key = UncheckedKey::new(dependency.into(), BlockHash::zero()); 
        for (key, id) in by_key.range(key..) {
            println!("36");
            let block_hash: BlockHash = dependency.into();
            if predicate() && block_hash == key.previous {
                println!("37");
                let entry = by_id.get(id).unwrap();
                action(&entry.key, &entry.info);
            }
        }
        println!("38");
    }

    pub fn query_impl(&self, hash: &HashOrAccount) {
        let mutex = Arc::clone(&self.mutable);
        let delete_queue = Arc::new(Mutex::new(VecDeque::new()));
        let delete_queue_copy = Arc::clone(&delete_queue); 
        self.for_each2(hash, Box::new(move |key, info| {
            let mut dq = delete_queue_copy.lock().unwrap();
            dq.push_back(key.clone());
            //let lock = mutex.lock().unwrap();
            //lock.stats.inc(StatType::Unchecked, DetailType::Satisfied, Direction::In);
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
    next_id: usize, 
    by_key: BTreeMap<UncheckedKey, usize>,
    by_id: BTreeMap<usize, Entry>,
}

impl EntriesContainer {
    fn new() -> Self {
        Self {
            by_id: BTreeMap::new(),
            by_key: BTreeMap::new(),
            next_id: 0,
        }
    }

    pub fn insert(&mut self, entry: Entry) -> bool {
        match self.by_key.get(&entry.key) {
            Some(key) => {
                false
            }
            None => {
                self.by_key.insert(entry.clone().key, self.next_id);

                self.by_id.insert(self.next_id, entry.clone());

                self.next_id += 1;

                true
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.next_id == 0
    }

    fn size(&self) -> usize {
        self.next_id
    }

    fn pop_front(&mut self) {
        let entry = self.by_id.get(&(0)).unwrap().clone();
        self.by_id.pop_first();
        self.by_key.remove(&entry.key);
        //self.next_id -= 1;
    }

    fn clear(&mut self) {
        self.by_id = BTreeMap::new();
        self.by_key = BTreeMap::new();
        self.next_id = 0;
    }
}

#[cfg(test)]
mod tests {
    use mock_instant::MockClock;

    use super::*;

    #[test]
    fn empty_container() {
        let container = EntriesContainer::new();
        assert_eq!(container.next_id, 0);
        assert_eq!(container.by_id.len(), 0);
        assert_eq!(container.by_key.len(), 0);
    }

    #[test]
    fn insert_one_entry() {
        let mut container = EntriesContainer::new();

        let entry = Entry::new(UncheckedKey::new(BlockHash::default(), BlockHash::default()), UncheckedInfo::default());
        let new_insert = container.insert(entry.clone());

        assert_eq!(container.next_id, 1);
        assert_eq!(container.by_id.len(), 1);
        assert_eq!(container.by_id.get(&0).unwrap(), &entry);
        assert_eq!(container.by_key.len(), 1);
        assert_eq!(container.by_key.get(&entry.key).unwrap(), &0);
        assert_eq!(new_insert, true);
    }

    #[test]
    fn insert_two_entries_with_same_key() {
        let mut container = EntriesContainer::new();

        let entry = Entry::new(UncheckedKey::new(BlockHash::default(), BlockHash::default()), UncheckedInfo::default());
        let new_insert1 = container.insert(entry.clone());
        let new_insert2 = container.insert(entry.clone());

        assert_eq!(container.next_id, 1);
        assert_eq!(container.by_id.len(), 1);
        assert_eq!(container.by_key.len(), 1);
        assert_eq!(new_insert1, true);
        assert_eq!(new_insert2, false);
    }

    #[test]
    fn insert_two_entries_with_different_key() {
        let mut container = EntriesContainer::new();

        let entry1 = Entry::new(UncheckedKey::new(BlockHash::default(), BlockHash::default()), UncheckedInfo::default());
        let entry2 = Entry::new(UncheckedKey::new(BlockHash::random(), BlockHash::default()), UncheckedInfo::default());
        let new_insert1 = container.insert(entry1.clone());
        let new_insert2 = container.insert(entry2.clone());

        assert_eq!(container.next_id, 2);
        assert_eq!(container.by_id.len(), 2);
        assert_eq!(container.by_key.len(), 2);
        assert_eq!(new_insert1, true);
        assert_eq!(new_insert2, true);
    }

    #[test]
    fn pop_front() {
        let mut container = EntriesContainer::new();

        let entry1 = Entry::new(UncheckedKey::new(BlockHash::default(), BlockHash::default()), UncheckedInfo::default());
        let entry2 = Entry::new(UncheckedKey::new(BlockHash::random(), BlockHash::default()), UncheckedInfo::default());
        let new_insert1 = container.insert(entry1.clone());
        let new_insert2 = container.insert(entry2.clone());

        container.pop_front();

        assert_eq!(container.next_id, 2);
        assert_eq!(container.by_id.len(), 1);
        assert_eq!(container.by_id.get(&1).unwrap(), &entry2);
        assert_eq!(container.by_key.len(), 1);
        assert_eq!(container.by_key.get(&entry2.key).unwrap(), &1);
    }

    /*#[test]
    fn trimming_empty_container_does_nothing() {
        let mut container = EntriesContainer::new();
        assert_eq!(container.trim(Duration::from_secs(1)), false);
    }

    #[test]
    fn dont_trim_if_upper_bound_not_reached() {
        let mut container = EntriesContainer::new();
        container.insert(Account::from(1), Instant::now());
        assert_eq!(container.trim(Duration::from_secs(1)), false);
    }

    #[test]
    fn trim_if_upper_bound_reached() {
        let mut container = EntriesContainer::new();
        container.insert(Account::from(1), Instant::now());
        MockClock::advance(Duration::from_millis(1001));
        assert_eq!(container.trim(Duration::from_secs(1)), true);
        assert_eq!(container.len(), 0);
    }

    #[test]
    fn trim_multiple_entries() {
        let mut container = EntriesContainer::new();

        container.insert(Account::from(1), Instant::now());
        container.insert(Account::from(2), Instant::now());

        MockClock::advance(Duration::from_millis(500));
        container.insert(Account::from(3), Instant::now());

        MockClock::advance(Duration::from_millis(1001));
        container.insert(Account::from(4), Instant::now());

        assert_eq!(container.trim(Duration::from_secs(1)), true);
        assert_eq!(container.len(), 1);
        assert_eq!(container.iter().next().unwrap(), &Account::from(4));
        assert_eq!(container.by_time.len(), 1);
    }*/
}
