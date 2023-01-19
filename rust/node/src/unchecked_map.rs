use std::collections::{BTreeMap, HashMap, VecDeque};
use std::collections::btree_map::Iter;
use std::hash::Hash;
use std::ops::Deref;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::{mem, thread};
use std::borrow::{Borrow, BorrowMut};
use std::cell::RefCell;
use std::thread::{current, JoinHandle, spawn, Thread};
use rsnano_core::{Account, BlockHash, HashOrAccount, UncheckedInfo, UncheckedKey};
use rsnano_store_lmdb::{get, LmdbStore, LmdbWriteTransaction};
use rsnano_store_traits::{Store, Transaction, UncheckedStore, WriteTransaction};

const MEM_BLOCK_COUNT_MAX: usize = 256000;

type Insert = (HashOrAccount, UncheckedInfo);
type Query = HashOrAccount;

#[derive(Clone)]
enum Op {
    Insert(Insert),
    Query(Query),
}

/*struct ItemVisitor {
    transaction: LmdbWriteTransaction,
    unchecked: MyStruct,
}

impl ItemVisitor {
    fn new(transaction: LmdbWriteTransaction, unchecked: MyStruct) -> Self {
        Self {
            transaction,
            unchecked,
        }
    }

    fn insert(&mut self, item: Insert) {
        //self.unchecked.insert_impl()
    }

    fn query(&mut self, item: Insert) {
        //self.query_impl()
    }
}*/

struct ItemVisitor<'a> {
    unchecked: &'a UncheckedMap,
    transaction: LmdbWriteTransaction,
}

impl<'a> ItemVisitor<'a> {
    fn visit_insert(&self, item: &Insert) {
        //let key = item.key.as_bytes();
        //let value = item.value.as_bytes();
        //self.transaction.put(key, value);
    }

    fn visit_query(&self, item: &Query) {
        /*let key = item.key.as_bytes();
        let result = self.transaction.get(key);
        if let Ok(value) = result {
            item.result.store(value);
        } else {
            item.result.store(None);
        }*/
    }
}

/*void nano::unchecked_map::item_visitor::operator() (insert const & item)
{
	auto const & [dependency, info] = item;
	unchecked.insert_impl (transaction, dependency, info);
}

void nano::unchecked_map::item_visitor::operator() (query const & item)
{
	unchecked.query_impl (transaction, item.hash);
}*/

/*{
	public:
		item_visitor (unchecked_map & unchecked, nano::write_transaction const & transaction);
		void operator() (insert const & item);
		void operator() (query const & item);
		unchecked_map & unchecked;
		nano::write_transaction const & transaction;
	};
	*/

pub struct UncheckedMap {
    data: Arc<Mutex<i32>>,
    handle: Option<thread::JoinHandle<()>>,
    store: Arc<LmdbStore>,
    buffer: Arc<Mutex<VecDeque<Op>>>,
    back_buffer: Arc<Mutex<VecDeque<Op>>>,
    condition: Arc<Mutex<Condvar>>,
    writing_back_buffer: Arc<Mutex<bool>>,
    stopped: Arc<Mutex<bool>>,
    entries: Arc<Mutex<EntryContainer>>,
    disable_delete: Arc<Mutex<bool>>,
}

impl UncheckedMap {
    pub fn new(store: Arc<LmdbStore>, disable_delete: bool) -> UncheckedMap {
        let data = Arc::new(Mutex::new(0));
        let buffer = Arc::new(Mutex::new(VecDeque::new()));
        let back_buffer = Arc::new(Mutex::new(VecDeque::new()));
        let condition = Arc::new(Mutex::new(Condvar::new()));
        let writing_back_buffer = Arc::new(Mutex::new(false));
        let stopped = Arc::new(Mutex::new(false));
        let entries = Arc::new(Mutex::new(EntryContainer::new()));
        let disable_delete = Arc::new(Mutex::new(disable_delete));
        let my_struct = UncheckedMap {
            data: data.clone(),
            handle: None,
            store: store.clone(),
            buffer: buffer.clone(),
            back_buffer: back_buffer.clone(),
            condition: condition.clone(),
            writing_back_buffer: writing_back_buffer.clone(),
            stopped: stopped.clone(),
            entries: entries.clone(),
            disable_delete: disable_delete.clone(),
        };

        let handle = thread::spawn(move || {
            let mut data = data.lock().unwrap();
            let mut back_buffer = back_buffer.lock().unwrap();
            let mut condition = condition.lock().unwrap();
            let mut writing_back_buffer = writing_back_buffer.lock().unwrap();
            let mut stopped = stopped.lock().unwrap();
            let mut disable_delete = *disable_delete.lock().unwrap();
            while !*stopped {
                let mut buffer = buffer.lock().unwrap();
                let mut entries = entries.lock().unwrap();
                let mut store = store.clone();
                if !buffer.is_empty() {
                    mem::swap(&mut buffer, &mut back_buffer);
                    *writing_back_buffer = true;
                    //self.write_buffer(&back_buffer);
                    UncheckedMap::write_buffer(&back_buffer, store, entries, disable_delete);
                    *writing_back_buffer = false;
                } else {
                    condition.notify_all(); // Notify flush()
                    condition.wait(buffer);
                }
                *data += 1;
            }
        });
        let my_struct = UncheckedMap {
            handle: Some(handle),
            ..my_struct
        };
        my_struct
    }

    fn get_data(&self) -> i32 {
        let data = self.data.lock().unwrap();
        *data
    }

    fn join_thread(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }

    fn write_buffer(back_buffer: &VecDeque<Op>, store: Arc<LmdbStore>, entries: MutexGuard<EntryContainer>, disable_delete: bool) {
        let mut transaction = store.tx_begin_write().unwrap();
        //let visitor = ItemVisitor {
            //unchecked: self,
            //transaction,
        //};
        for item in back_buffer {
            match item {
                Op::Insert(i) => UncheckedMap::insert_impl(&mut transaction, i.0.clone(), i.1.clone(), store.clone(), entries.clone()), //visitor.visit_insert(i),
                Op::Query(q) => UncheckedMap::query_impl(&mut transaction, BlockHash::from(q.number()), store.clone(), entries.clone(), disable_delete),
            }
        }
    }

    pub fn insert_impl(transaction: &mut dyn WriteTransaction, dependency: HashOrAccount, info: UncheckedInfo, store: Arc<LmdbStore>, mut entries: EntryContainer) {
        //let mut entries = self.entries.entries.lock().unwrap().clone();
        /*if entries.is_empty() {//&& (self.use_memory)() {
            let mut entries_new = EntryContainer::new();//.entries.lock().unwrap();
            let mut mutex = entries_new.entries.lock().unwrap();
            let action: Box<dyn Fn(&UncheckedKey, &UncheckedInfo)> = Box::new(move |key, info| {
                let entry = Entry {
                    key: key.clone(),
                    info: info.clone(),
                };
                //mutex.insert(0, entry);
            });
            //let predicate: Box<dyn Fn() -> bool> = Box::new(|| &mutex.len() < &MEM_BLOCK_COUNT_MAX);
            self.for_each(transaction.deref(), action);
            //entries = entries_new;
        }*/
        if entries.is_empty() {
            let block = info.clone().block.unwrap();
            store.unchecked_store.put(transaction, &dependency, &info);
        }
        else {
            let key = UncheckedKey::new(info.previous(), info.hash());
            let entry = Entry {
                key,
                info
            };
            entries.insert(entry);
            while entries.size() > MEM_BLOCK_COUNT_MAX
            {
                entries.pop_front();
                //entries->template get<tag_sequenced> ().pop_front ();
            }
        }
    }

    fn query_impl(transaction: &mut dyn WriteTransaction, hash: BlockHash, store: Arc<LmdbStore>, entries: EntryContainer, disable_delete: bool) {
        let mut delete_queue: VecDeque<UncheckedKey> = VecDeque::new();
        if !disable_delete {
            if entries.is_empty() {
                let (mut i, n) = store.unchecked_store.equal_range(transaction.txn(), hash);
                while !i.is_end() {
                    delete_queue.push_back(i.current().unwrap().0.clone());
                    i.next();
                }
            }
            else {
                let it = store.unchecked_store.lower_bound(transaction.txn(), &UncheckedKey::new(hash, BlockHash::zero()));
                for (i, e) in entries.entries.iter() {
                    delete_queue.push_back(e.clone().key);
                }
            }
            for key in delete_queue {
                store.unchecked_store.del(transaction, &key);
            }
        }
    }

    /*void nano::unchecked_map::query_impl (nano::write_transaction const & transaction, nano::block_hash const & hash)
    {
    nano::lock_guard<std::recursive_mutex> lock{ entries_mutex };

    std::deque<nano::unchecked_key> delete_queue;
    for_each (transaction, hash, [this, &delete_queue] (nano::unchecked_key const & key, nano::unchecked_info const & info) {
    delete_queue.push_back (key);
    satisfied (info);
    });
    if (!disable_delete)
    {
    for (auto const & key : delete_queue)
    {
    del (transaction, key);
    }
    }
    }*/

    pub fn exists(&self, transaction: &mut dyn WriteTransaction, key: &UncheckedKey) -> bool {
        let entries = self.entries.lock().unwrap();
        if entries.is_empty()
        {
            return self.store.unchecked().exists (transaction.txn(), key);
        }
        else
        {
            if let Some(i) = entries.by_key.get(key) {
                return true;
            }
            else {
                return false;
            }
        }
    }

    pub fn stop(&mut self) {
        let mut stopped = *self.stopped.lock().unwrap();
        if !stopped {
            stopped = true;
            self.condition.lock().unwrap().notify_all();
        }
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }

    pub fn trigger(&mut self, dependency: HashOrAccount) {
        let mut buffer = self.buffer.lock().unwrap();
        buffer.push_back(Op::Query(dependency));
        //debug_assert (buffer.back ().which () == 1); // which stands for "query".
        //lock.unlock ();
        self.condition.lock().unwrap().notify_all (); // Notify run ()
    }

    pub fn del(&mut self, transaction: &mut dyn WriteTransaction, key: &UncheckedKey) {
        let mut entries = self.entries.lock().unwrap();
        if entries.is_empty() {
            self.store.unchecked_store.del(transaction, key);
        }
        else {
            let erase = entries.by_key.remove(key);
            debug_assert!(erase.is_some());
        }
    }

    pub fn clear(&mut self, transaction: &mut dyn WriteTransaction) {
        let mut entries = self.entries.lock().unwrap();
        if entries.is_empty() {
            self.store.unchecked_store.clear(transaction);
        }
        else {
            entries.clear();
        }
        /*nano::lock_guard<std::recursive_mutex> lock{ entries_mutex };
	if (entries == nullptr)
	{
		store.unchecked ().clear (transaction);
	}
	else
	{
		entries->clear ();
	}*/
    }

    pub fn put(&mut self, dependency: HashOrAccount, info: UncheckedInfo) {
        self.buffer.lock().unwrap().push_back(Op::Insert((dependency, info)));
        self.condition.lock().unwrap().notify_all();
    }

    pub fn get(&mut self, transaction: &dyn Transaction, hash: BlockHash) -> Vec<UncheckedInfo> {
        let mut result = RefCell::new(Vec::new());
        /*self.for_each2(transaction, hash, Box::new(|k, i| {
            result.borrow_mut().push(i.clone());
        }), Box::new(|| true));*/
        result.into_inner()
        //std::vector<nano::unchecked_info> nano::unchecked_map::get (nano::transaction const & transaction, nano::block_hash const & hash)
        //{
            //std::vector<nano::unchecked_info> result;
            //for_each (transaction, hash, [&result] (nano::unchecked_key const & key, nano::unchecked_info const & info) {
            //result.push_back (info);
        //});
            //return result;
        //}
    }

    pub fn for_each2(&mut self, transaction: &dyn Transaction, dependency: BlockHash, action: Box<dyn Fn(&UncheckedKey, &UncheckedInfo)>, predicate: Box<dyn Fn() -> bool>) {
        let entries = self.entries.lock().unwrap();
        //let dependency = BlockHash::from_bytes(*dependency.as_bytes());
        if entries.is_empty()
        {
            let (mut i, n) = self.store.unchecked_store.equal_range(transaction, dependency);
            while !i.is_end() {
                if predicate() && i.current().unwrap().0.hash == dependency {
                    let (key, info) = i.current().unwrap();
                    action(key, info);
                }
                i.next();
            }
        }
        else
        {
            for (_, entry) in entries.entries.iter() { // predicate
                if predicate() && entry.key.previous == dependency {
                    action(&entry.key, &entry.info);
                }
            }
        }
    }

    pub fn for_each1(&mut self, transaction: &dyn WriteTransaction, action: Box<dyn Fn(&UncheckedKey, &UncheckedInfo)>) {
        let mut entries = self.entries.lock().unwrap().clone();
        if entries.is_empty() {
            let mut it = self.store.unchecked().begin(transaction.txn());
            while !it.is_end() {
                if entries.size() < MEM_BLOCK_COUNT_MAX { // predicate
                    let (key, info) = it.current().unwrap();
                    action(key, info);
                    let entry = Entry {
                        key: key.clone(),
                        info: info.clone(),
                    };
                    entries.insert(entry);
                }
            }
        }
        else {
            //let entries = self.entries.entries.clone();
            for (_, entry) in entries.entries.iter() { // predicate
                if entries.entries.len() < MEM_BLOCK_COUNT_MAX {
                    action(&entry.key, &entry.info);
                }
            }
        }
    }

    pub fn flush(&mut self) {
        while !*self.stopped.lock().unwrap() && (self.buffer.lock().unwrap().is_empty() && self.back_buffer.lock().unwrap().is_empty() && !*self.writing_back_buffer.lock().unwrap()) {
            self.condition.lock();
        }
        //nano::unique_lock<nano::mutex> lock{ mutex };
        //condition.wait (lock, [this] () {
            //return stopped || (buffer.empty () && back_buffer.empty () && !writing_back_buffer);
        //});
    }

    pub fn count(&self, transaction: &dyn Transaction) -> usize {
        let entries = self.entries.lock().unwrap();
        if entries.is_empty() {
            return self.store.unchecked_store.count(transaction) as usize;
        }
        else {
            return entries.size();
        }
    }
}

/*pub struct UncheckedMap {
    store: Arc<LmdbStore>,
    disable_delete: bool,
    buffer: Arc<Mutex<VecDeque<Op>>>,
    back_buffer: Arc<Mutex<VecDeque<Op>>>,
    entries: EntryContainer,
    stopped: bool,
    writing_back_buffer: bool,
    use_memory: Box<dyn Fn() -> bool + Send>,
    condition: Condvar,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl UncheckedMap {
    pub fn new(store: Arc<LmdbStore>, disable_delete: bool) -> Self {
        let use_memory = Box::new(move || { true });
        let mut my_struct = Self {
            store,
            disable_delete,
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            back_buffer: Arc::new(Mutex::new(VecDeque::new())),
            entries: EntryContainer::new(),
            stopped: false,
            writing_back_buffer: false,
            use_memory,
            condition: Condvar::new(),
            thread_handle: None,
        };


        //let stopped = my_struct.stopped;
        //let mut writing_back_buffer = my_struct.writing_back_buffer;
        //let condition = my_struct.condition;
        /*let my_struct = Arc::new(Mutex::new(my_struct)).lock().unwrap();
        let handle = thread::Builder::new().name("my_thread".into()).spawn(move || {
            while !stopped {
                let mut buffer = my_struct.buffer.lock().unwrap();
                let mut back_buffer = my_struct.buffer.lock().unwrap();
                //let mut buffer_guard = self.buffer.lock().unwrap();
                if !buffer.is_empty() {
                    //let mut back_buffer_guard = self.back_buffer.lock().unwrap();
                    mem::swap(&mut buffer, &mut back_buffer);
                    //writing_back_buffer = true;
                    //self.write_buffer(&back_buffer);
                    //writing_back_buffer = false;
                    back_buffer.clear();
                    drop(back_buffer);
                } else {
                    condition.notify_all(); // Notify flush()
                    condition.wait(buffer);
                }
            }
        });
        my_struct.thread_handle = Some(handle.unwrap());*/
        my_struct
    }

    pub fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            self.condition.notify_all();
        }
        if let Some(handle) = self.thread_handle.take() {
            handle.join().unwrap();
        }
    }

    /*void nano::unchecked_map::stop ()
    {
    nano::unique_lock<nano::mutex> lock{ mutex };
    if (!stopped)
    {
    stopped = true;
    condition.notify_all (); // Notify flush (), run ()
    }
    }*/

    fn run(&mut self) {
        while !self.stopped {
            let mut buffer = self.buffer.lock().unwrap();
            let mut back_buffer = self.buffer.lock().unwrap();
            if !buffer.is_empty() {
                let mut back_buffer = self.back_buffer.lock().unwrap();
                mem::swap(&mut buffer, &mut back_buffer);
                self.writing_back_buffer = true;
                //self.write_buffer(&back_buffer);
                self.writing_back_buffer = false;
                back_buffer.clear();
            } else {
                self.condition.notify_all(); // Notify flush()
                self.condition.wait(buffer);
            }
        }
    }

    /*pub fn run(&mut self) {
        //nano::thread_role::set (nano::thread_role::name::unchecked);
        //let mutex = Mutex::new(());
        //let mut lock = mutex.lock().unwrap();
        /*while !self.stopped {
            if !self.buffer.lock().unwrap().is_empty() {
                std::mem::swap(&mut self.buffer, &mut self.back_buffer);
                self.writing_back_buffer = true;
                //self.write_buffer(&self.back_buffer);
                self.writing_back_buffer = false;
                self.back_buffer.lock().unwrap().clear();
            } else {
                self.condition.notify_all(); // Notify flush()
                self.condition.wait(self.buffer.lock().unwrap());
            }
        }*/
        let buffer = self.buffer.lock().unwrap();
        let stopped = self.stopped.lock().unwrap();
        let handle = thread::Builder::new()
            .name("my_thread".into())
            .spawn(move || {
                // thread code here
                while !self.stopped {
                    if !buffer.is_empty() {
                        std::mem::swap(&mut self.buffer, &mut self.back_buffer);
                        self.writing_back_buffer = true;
                        //self.write_buffer(&self.back_buffer);
                        self.writing_back_buffer = false;
                        self.back_buffer.lock().unwrap().clear();
                    } else {
                        self.condition.notify_all(); // Notify flush()
                        self.condition.wait(self.buffer.lock().unwrap());
                    }
            }})
            .unwrap();
        //slet thread_id = handle.thread().id();
    }*/

    /*pub fn run(&self) {
        let mut back_buffer = self.back_buffer.clone();
        let mut buffer = self.buffer.clone();
        let handle = Arc::new(Mutex::new(self));
        thread::spawn(move || {
            let handle = handle.clone();
            let handle = handle.lock().unwrap();
            while !self.stopped {
                let mut buffer_mutex = buffer.lock().unwrap();
                let mut back_buffer_mutex = buffer.lock().unwrap();
                if buffer_mutex.is_empty().clone() {
                    std::mem::swap(&mut back_buffer, &mut buffer.clone());
                    self.write_buffer();
                    handle.writing_back_buffer = false;
                    back_buffer_mutex.clear();
                }
                else {
                    self.condition.notify_all();
                    let m = self.condition.wait(buffer_mutex).unwrap();
                    //and_then(| | {
                        handle.stopped = m.is_empty();
                        //Ok(())
                    //});
                }
            }
        });
    }*/

    /*nano::thread_role::set (nano::thread_role::name::unchecked);
	nano::unique_lock<nano::mutex> lock{ mutex };
	while (!stopped)
	{
		if (!buffer.empty ())
		{
			back_buffer.swap (buffer);
			writing_back_buffer = true;
			lock.unlock ();
			write_buffer (back_buffer);
			lock.lock ();
			writing_back_buffer = false;
			back_buffer.clear ();
		}
		else
		{
			condition.notify_all (); // Notify flush ()
			condition.wait (lock, [this] () {
				return stopped || !buffer.empty ();
			});
		}
	}*/

    pub fn write_buffer(&self) {
        let transaction = self.store.tx_begin_write().unwrap();
        //let item_visitor = ItemVisitor::new(transaction, self.clone());
    }

    /*void nano::unchecked_map::write_buffer (decltype (buffer) const & back_buffer)
    {
    auto transaction = store.tx_begin_write ();
    item_visitor visitor{ *this, *transaction };
    for (auto const & item : back_buffer)
    {
    boost::apply_visitor (visitor, item);
    }
    }*/

    pub fn insert_impl(&mut self, transaction: &mut dyn WriteTransaction, dependency: HashOrAccount, info: UncheckedInfo) {
        //let mut entries = self.entries.entries.lock().unwrap().clone();
        if self.entries.is_empty() {//&& (self.use_memory)() {
            let mut entries_new = EntryContainer::new();//.entries.lock().unwrap();
            //let mut mutex = entries_new.entries.lock().unwrap();
            let action: Box<dyn Fn(&UncheckedKey, &UncheckedInfo)> = Box::new(move |key, info| {
                let entry = Entry {
                    key: key.clone(),
                    info: info.clone(),
                };
                //mutex.insert(0, entry);
            });
            //let predicate: Box<dyn Fn() -> bool> = Box::new(|| &mutex.len() < &MEM_BLOCK_COUNT_MAX);
            self.for_each(transaction.deref(), action);
            //entries = entries_new;
        }
        if self.entries.is_empty() {
            let block = info.clone().block.unwrap();
            self.store.unchecked_store.put(transaction, &dependency, &info);
        }
        else {
            let key = UncheckedKey::new(info.previous(), info.hash());
            let entry = Entry {
                key,
                info
            };
            self.entries.insert(entry);
            while self.entries.size() > MEM_BLOCK_COUNT_MAX
            {
                self.entries.pop_front();
                //entries->template get<tag_sequenced> ().pop_front ();
            }
        }
    }

    /*void nano::unchecked_map::insert_impl (nano::write_transaction const & transaction, nano::hash_or_account const & dependency, nano::unchecked_info const & info)
{
	nano::lock_guard<std::recursive_mutex> lock{ entries_mutex };
	// Check if we should be using memory but the memory container hasn't been constructed i.e. we're transitioning from disk to memory.
	if (entries == nullptr && use_memory ())
	{
		auto entries_new = std::make_unique<typename decltype (entries)::element_type> ();
		for_each (
		transaction, [&entries_new] (nano::unchecked_key const & key, nano::unchecked_info const & info) { entries_new->template get<tag_root> ().insert ({ key, info }); }, [&] () { return entries_new->size () < mem_block_count_max; });
		clear (transaction);
		entries = std::move (entries_new);
	}
	if (entries == nullptr)
	{
		auto block{ info.get_block () };
		store.unchecked ().put (transaction, dependency, { block });
	}
	else
	{
		nano::unchecked_key key{ dependency, info.get_block ()->hash () };
		entries->template get<tag_root> ().insert ({ key, info });
		while (entries->size () > mem_block_count_max)
		{
			entries->template get<tag_sequenced> ().pop_front ();
		}
	}
    }*/

    fn for_each<F: Fn(&UncheckedKey, &UncheckedInfo)> (&mut self, transaction: &dyn WriteTransaction, action: F) {
        //let mut entries = self.entries.entries.lock().unwrap().clone();
        if self.entries.is_empty() {
            let mut it = self.store.unchecked().begin(transaction.txn());
            while !it.is_end() {
                if self.entries.size() < MEM_BLOCK_COUNT_MAX { // predicate
                    let (key, info) = it.current().unwrap();
                    //action(key, info);
                    let entry = Entry {
                        key: key.clone(),
                        info: info.clone(),
                    };
                    self.entries.insert(entry);
                }
            }
        }
        else {
            let entries = self.entries.entries.clone();
            for (_, entry) in entries.iter() { // predicate
                if entries.len() < MEM_BLOCK_COUNT_MAX {
                    action(&entry.key, &entry.info);
                }
            }
        }
    }

    /*void nano::unchecked_map::for_each (
nano::transaction const & transaction, std::function<void (nano::unchecked_key const &, nano::unchecked_info const &)> action, std::function<bool ()> predicate)
{
	nano::lock_guard<std::recursive_mutex> lock{ entries_mutex };
	if (entries == nullptr)
	{
		for (auto [i, n] = store.unchecked ().full_range (transaction); predicate () && i != n; ++i)
		{
			action (i->first, i->second);
		}
	}
	else
	{
		for (auto i = entries->begin (), n = entries->end (); predicate () && i != n; ++i)
		{
			action (i->key, i->info);
		}
	}
}*/
}*/

#[derive(Clone)]
pub struct Entry {
    key: UncheckedKey,
    info: UncheckedInfo,
}

#[derive(Default, Clone)]
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
