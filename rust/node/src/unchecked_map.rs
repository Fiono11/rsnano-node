use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use rsnano_core::{BlockHash, HashOrAccount, UncheckedInfo, UncheckedKey};
use rsnano_store_lmdb::LmdbStore;
use rsnano_store_traits::{Store, Transaction, UncheckedStore, WriteTransaction};

const MEM_BLOCK_COUNT_MAX: usize = 256000;

type Insert = (HashOrAccount, UncheckedInfo);
type Query = HashOrAccount;

#[derive(Clone, Debug)]
enum Op {
    Insert(Insert),
    Query(Query),
}

pub struct StateUncheckedMapThread {
    condition: Condvar,
    mutable: Mutex<ThreadMutableData>,
    store: Arc<LmdbStore>,
    disable_delete: bool,
    use_memory: Box<dyn Fn() -> bool + Send + Sync>,
}

impl StateUncheckedMapThread {
    fn insert_impl(&self, data: &mut ThreadMutableData, transaction: &mut dyn WriteTransaction, dependency: HashOrAccount, info: UncheckedInfo) {
        println!("10");
        if data.entries.is_empty() && (self.use_memory)() {
            println!("AAAAa");
            let mut entries_new = EntryContainer::new();
            //let a = |entries_new: &mut EntryContainer, key: &UncheckedKey, info: &UncheckedInfo| {
                //entries_new.insert(Entry { key: key.clone(), info: info.clone()});
            //};
            //self.for_each2(transaction.txn(), BlockHash::from_bytes(*dependency.as_bytes()), Box::new(a), Box::new(|| true));

            if data.entries.is_empty()
            {
                println!("E");
                let (mut i, n) = self.store.unchecked_store.equal_range(transaction, BlockHash::from_bytes(*dependency.as_bytes()));
                while !i.is_end() {
                    if entries_new.size() < MEM_BLOCK_COUNT_MAX && i.current().unwrap().0.hash == dependency {
                        println!("F");
                        let (key, info) = i.current().unwrap();
                        //action(&mut lock.entries, key, info);
                    }
                    i.next();
                }
            }
            else
            {
                let entries = lock.entries.clone();
                for (_, entry) in entries.entries.iter() { // predicate
                    println!("G");
                    if predicate() && entry.key.previous == dependency {
                        println!("H");
                        action(&mut lock.entries, &entry.key, &entry.info);
                    }
                }
            }

            self.clear(transaction);
		    data.entries = entries_new;
            println!("entries: {:?}", data.entries);
        }
        if data.entries.is_empty() {
            println!("BBBBB");
            self.store.unchecked().put(transaction, &dependency, &info);
        }
        else {
            println!("15");
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
    }

    fn query_impl(&self, data: &mut ThreadMutableData, transaction: &mut dyn WriteTransaction, hash: BlockHash) {
        println!("9");
        let mut delete_queue: VecDeque<UncheckedKey> = VecDeque::new();
        if !self.disable_delete {
            println!("11");
            if data.entries.is_empty() {
                println!("12");
                let (mut i, n) = self.store.unchecked_store.equal_range(transaction.txn(), hash);
                while !i.is_end() {
                    delete_queue.push_back(i.current().unwrap().0.clone());
                    i.next();
                }
            }
            else {
                println!("13");
                let it = self.store.unchecked_store.lower_bound(transaction.txn(), &UncheckedKey::new(hash, BlockHash::zero()));
                for (i, e) in data.entries.entries.iter() {
                    delete_queue.push_back(e.clone().key);
                }
            }
            println!("14");
            for key in delete_queue {
                self.store.unchecked_store.del(transaction, &key);
            }
        }
    }

    fn write_buffer(&self, data: &mut ThreadMutableData) {
        println!("20");
        let mut transaction = self.store.tx_begin_write().unwrap();
        println!("24");
        let back_buffer = &data.back_buffer.clone();
        println!("21");
        for item in back_buffer {
            match item {
                Op::Insert(i) => {
                    println!("22");
                    self.insert_impl(data, &mut transaction, i.0.clone(), i.1.clone());
                    println!("CCCCC");
                },
                Op::Query(q) => {
                    println!("23");
                    self.query_impl(data, &mut transaction, BlockHash::from(q.number()));
                },
            }
        }
    }

    fn run(&self) {
        //let mut transaction = self.store.tx_begin_write().unwrap();
        let mut lk = self.mutable.lock().unwrap();
        println!("1");
        while !lk.stopped {
            println!("2");
            if !lk.buffer.is_empty() {
                println!("buffer1: {:#?}", &lk.buffer);
                println!("back_buffer1: {:#?}", &lk.back_buffer);
                println!("3");
                let temp = lk.buffer.clone();
                lk.buffer = lk.back_buffer.clone();
                lk.back_buffer = temp;
                //let mut buffer = &mut lk.buffer;
                //let mut back_buffer = &mut lk.back_buffer;
                //let back_buffer = lk.back_buffer.clone();
                //let temp = std::mem::replace(&mut lk.buffer, back_buffer);
                //lk.back_buffer = temp;
                //let temp = &lk.buffer;
                //lk.buffer = lk.back_buffer.clone();
                //lk.back_buffer = temp;
                //unsafe { std::ptr::swap(lk.buffer.as, back_buffer); }
                //mem::swap(&mut lk.buffer, &mut lk.back_buffer);
                println!("buffer2: {:#?}", &lk.buffer);
                println!("back_buffer2: {:#?}", &lk.back_buffer);
                println!("4");
                lk.writing_back_buffer = true;
                //drop(lk);
                self.write_buffer(&mut lk);
                //lk = self.mutable.lock().unwrap();
                println!("buffer3: {:#?}", &lk.buffer);
                println!("back_buffer3: {:#?}", &lk.back_buffer);
                println!("5");
                lk.writing_back_buffer = false;
                lk.back_buffer.clear();
                //lk.buffer.clear();
            } else {
                println!("6");
                self.condition.notify_all();
                let stopped = lk.stopped;
                let mut buffer = lk.buffer.is_empty();
                lk = self.condition.wait(lk).unwrap();
                //lk = self.condition.wait_while(lk, |_| stopped == false || buffer == true).unwrap();
                println!("7");
                //break;
            }
        }
        println!("8");
    }

    pub fn exists(&self, transaction: &dyn Transaction, key: &UncheckedKey) -> bool {
        let lock = self.mutable.lock().unwrap();
        return if lock.entries.is_empty() {
            self.store.unchecked().exists(transaction, key)
        } else {
            if let Some(_) = lock.entries.by_key.get(key) {
                true
            } else {
                false
            }
        }
    }

    pub fn trigger(&self, dependency: HashOrAccount) {
        let mut lock = self.mutable.lock().unwrap();
        lock.buffer.push_back(Op::Query(dependency));
        self.condition.notify_all(); // Notify run ()
    }

    pub fn del(&self, transaction: &mut dyn WriteTransaction, key: &UncheckedKey) {
        let mut lock = self.mutable.lock().unwrap();
        if lock.entries.is_empty() {
            self.store.unchecked_store.del(transaction, key);
        }
        else {
            let erase = lock.entries.by_key.remove(key);
            debug_assert!(erase.is_some());
        }
    }

    pub fn clear(&self, transaction: &mut dyn WriteTransaction) {
        let mut lock = self.mutable.lock().unwrap();
        if lock.entries.is_empty() {
            self.store.unchecked_store.clear(transaction);
        }
        else {
            lock.entries.clear();
        }
    }

    pub fn put(&self, dependency: HashOrAccount, info: UncheckedInfo) {
        println!("!!!!!!!!!!!!!!!");
        let mut lock = self.mutable.lock().unwrap();
        lock.buffer.push_back(Op::Insert((dependency, info)));
        self.condition.notify_all();
    }

    pub fn get(&self, transaction: &dyn Transaction, dependency: BlockHash) -> Vec<UncheckedInfo> {
        let mut result = Vec::new();
        let lock = self.mutable.lock().unwrap();
        if lock.entries.is_empty()
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
            for (_, entry) in lock.entries.entries.iter() { // predicate
                if entry.key.previous == dependency {
                    //action(&entry.key, &entry.info);
                    result.push(entry.info.clone());
                }
            }
        }
        println!("result: {:?}", result);
        result
    }

    pub fn for_each2(&self, transaction: &dyn Transaction, dependency: BlockHash, mut action: Box<dyn FnMut(&mut EntryContainer, &UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        println!("DDDDDD");
        let mut lock = self.mutable.lock().unwrap();
        //let mut entries = lock.entries.clone();
        //let dependency = BlockHash::from_bytes(*dependency.as_bytes());
        if *&lock.entries.is_empty()
        {
            println!("E");
            let (mut i, n) = self.store.unchecked_store.equal_range(transaction, dependency);
            while !i.is_end() {
                if predicate() && i.current().unwrap().0.hash == dependency {
                    println!("F");
                    let (key, info) = i.current().unwrap();
                    action(&mut lock.entries, key, info);
                }
                i.next();
            }
        }
        else
        {
            let entries = lock.entries.clone();
            for (_, entry) in entries.entries.iter() { // predicate
                println!("G");
                if predicate() && entry.key.previous == dependency {
                    println!("H");
                    action(&mut lock.entries, &entry.key, &entry.info);
                }
            }
        }
    }

    pub fn for_each1(&self, transaction: &dyn Transaction, mut action: Box<dyn FnMut(&mut EntryContainer, &UncheckedKey, &UncheckedInfo)>) {
        let mut lock = self.mutable.lock().unwrap();
        //let mut entries = lock.entries.clone();
        if *&lock.entries.is_empty() {
            let mut it = self.store.unchecked().begin(transaction);
            while !it.is_end() {
                if &lock.entries.size() < &MEM_BLOCK_COUNT_MAX { // predicate
                    let (key, info) = it.current().unwrap();
                    action(&mut lock.entries, key, info);
                    let entry = Entry {
                        key: key.clone(),
                        info: info.clone(),
                    };
                    lock.entries.insert(entry);
                }
            }
        }
        else {
            //let entries = self.entries.entries.clone();
            for (_, entry) in lock.entries.entries.iter() { // predicate
                if lock.entries.entries.len() < MEM_BLOCK_COUNT_MAX {
                    action(&mut entries, &entry.key, &entry.info);
                }
            }
        }
        println!("entries2: {:?}", entries);
    }

    pub fn flush(&self) {
        let lock = self.mutable.lock().unwrap();
        let stopped = lock.stopped;
        let buffer = lock.buffer.is_empty();
        let back_buffer = lock.back_buffer.is_empty();
        let writing_back_buffer = lock.writing_back_buffer;
        self.condition.wait_while(lock, |_| !stopped && (!buffer &&
        !back_buffer && !writing_back_buffer)).unwrap();
    }

    pub fn count(&self, transaction: &dyn Transaction) -> usize {
        let lock = self.mutable.lock().unwrap();
        if lock.entries.is_empty() {
            return self.store.unchecked_store.count(transaction) as usize;
        }
        else {
            return lock.entries.size();
        }
    }
}

pub struct StateUncheckedMap {
    join_handle: Option<JoinHandle<()>>,
    pub thread: Arc<StateUncheckedMapThread>,
}

impl StateUncheckedMap {
    pub fn builder() -> Builder {
        Builder::new()
    }

    pub fn stop(&mut self) -> std::thread::Result<()> {
        {
            let mut lk = self.thread.mutable.lock().unwrap();
            lk.stopped = true;
        }

        if let Some(handle) = self.join_handle.take() {
            self.thread.condition.notify_one();
            handle.join()?;
        }
        Ok(())
    }

    pub fn action_callback(
        &self,
        callback: Box<dyn Fn(&UncheckedKey, &UncheckedInfo) + Send + Sync>,
    ) {
        //let mut lk = self.thread.callbacks.lock().unwrap();
        //lk.action_callback = Some(callback);
    }

    pub fn predicate_callback(&self, callback: Box<dyn Fn() -> bool + Send + Sync>) {
        //let mut lk = self.thread.callbacks.lock().unwrap();
        //lk.predicate_callback = Some(callback);
    }
}

struct Callbacks {
    action_callback: Option<Box<dyn Fn(&UncheckedKey, &UncheckedInfo) + Send + Sync>>,
    predicate_callback: Option<Box<dyn Fn() -> bool + Send + Sync>>,
}

struct ThreadMutableData {
    active: bool,
    stopped: bool,
    buffer: VecDeque<Op>,
    back_buffer: VecDeque<Op>,
    writing_back_buffer: bool,
    entries: EntryContainer,
}

#[derive(Default)]
pub struct Builder {
    store: Option<Arc<LmdbStore>>,
    disable_delete: bool,
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

    pub fn spawn(self) -> std::io::Result<StateUncheckedMap> {
        let thread = Arc::new(StateUncheckedMapThread {
            condition: Condvar::new(),
            mutable: Mutex::new(ThreadMutableData {
                active: false,
                stopped: false,
                buffer: Default::default(),
                back_buffer: Default::default(),
                writing_back_buffer: false,
                entries: Default::default()
            }),
            store: self.store.unwrap(),
            disable_delete: self.disable_delete,
            use_memory: Box::new(move || { true }),
        });

        let thread_clone = thread.clone();
        let join_handle = std::thread::Builder::new()
            .name("Unchecked".to_string())
            .spawn(move || {
                thread_clone.run();
            })?;

        Ok(StateUncheckedMap {
            join_handle: Some(join_handle),
            thread,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    key: UncheckedKey,
    info: UncheckedInfo,
}

#[derive(Default, Clone, Debug)]
pub struct EntryContainer {
    entries: BTreeMap<usize, Entry>, //BTreeSet?
    by_key: HashMap<UncheckedKey, usize>,
    //by_info:
    next_id: usize,
}

impl EntryContainer {
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
