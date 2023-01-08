use std::cmp::max;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::mem::size_of;
use std::ops::Deref;
use std::process::id;
use std::sync::{Arc, Mutex};
use std::thread::current;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use blake2::digest::typenum::private::Trim;
use rsnano_core::{Account, Amount, BlockHash, Root, u256_struct};
use rsnano_ledger::Ledger;
use rsnano_store_traits::{ReadTransaction, Transaction, WriteTransaction};
use crate::config::NodeConfig;
use crate::stats::DetailType::Send;
use crate::voting::VoteSpacing;
use primitive_types::U256;

pub const ONLINE_WEIGHT_QUORUM: u8 = 67;

pub struct OnlineReps {
    pub ledger: Arc<Ledger>,
    pub node_config: Arc<NodeConfig>,
    reps: EntryContainer,
    pub trended_m: Arc<Mutex<Amount>>,
    pub online_m: Arc<Mutex<Amount>>,
    pub minimum: Arc<Mutex<Amount>>,
}

impl OnlineReps {
    pub fn new(ledger: Arc<Ledger>, node_config: Arc<NodeConfig>) -> Self {

        let transaction = ledger.store.tx_begin_read().unwrap();

        let mut online_reps = Self {
            ledger,
            node_config,
            reps: EntryContainer::new(),
            trended_m: Arc::new(Mutex::new(Amount::zero())),
            online_m: Arc::new(Mutex::new(Amount::zero())),
            minimum: Arc::new(Mutex::new(Amount::zero())),
        };

        let mut mutex = online_reps.trended_m.lock().unwrap();
        *mutex = online_reps.calculate_trend(transaction.txn());
        std::mem::drop(mutex);
        online_reps
    }

    pub fn calculate_online(&self) -> Amount {
        let mut current = 0;
        for (i, e) in self.reps.entries.iter() {
            println!("REPS 3: {:?}", self.reps.by_account);
            println!("weight: {:?}", self.ledger.weight(&e.account));
            //if let Some(e) = &self.reps.entries.get(&i) {
                current += self.ledger.weight(&e.account).number();
            //}
        }
        Amount::new(current)
    }

    pub fn calculate_trend(&self, transaction_a: &dyn Transaction) -> Amount {
        let mut items = Vec::new();
        items.push(self.node_config.online_weight_minimum);
        let mut it = self.ledger.store.online_weight().begin(transaction_a);
        while !it.is_end() {
            items.push(*it.current().unwrap().1);
            it.next();
        }
        let median_idx = items.len() / 2;
        items.sort();
        let result = items[median_idx];
        return result;
    }

    pub fn observe(&mut self, rep_a: Account) {
        if self.ledger.weight(&rep_a).number() > 0 {
            let mut new_insert = true;
            //let mut mutex = self.reps.lock().unwrap();
            if let Some(id) = self.reps.by_account.get(&rep_a) {
                let old_time = self.reps.entries.get(id).unwrap().time;
                let mut ids = self.reps.by_time.get(&old_time).unwrap().clone();
                let index = ids.iter().position(|x| x == id).unwrap();
                ids.remove(index);
                self.reps.by_time.insert(old_time, ids.to_owned());
                self.reps.entries.remove(id).unwrap();
                self.reps.by_account.remove(&rep_a);
                new_insert = false;
            }
            let start = SystemTime::now();
            let since_the_epoch = start
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards");
            //let time = since_the_epoch.as_secs();
            let time = Instant::now();
            let entry = Entry {
                account: rep_a,
                time,
            };
            self.reps.insert(entry);
            let cutoff = since_the_epoch - Duration::from_secs(300);
            let trimmed = self.reps.trim(Duration::from_secs(300));
            if new_insert || trimmed {
                let mut mutex = self.online_m.lock().unwrap();
                *mutex = self.calculate_online();
            }
        }
    }

    pub fn sample(&mut self) {
        let mut transaction = self.ledger.store.tx_begin_write().unwrap();
        while self.ledger.store.online_weight().count(transaction.txn()) >= self.node_config.max_weight_samples {
            let oldest = self.ledger.store.online_weight().begin(transaction.txn());
            debug_assert!(oldest.as_ref().current().unwrap() != self.ledger.store.online_weight().rbegin(transaction.txn()).current().unwrap());
            self.ledger.store.online_weight().del(transaction.as_mut(), *oldest.current().unwrap().0);
        }
        let start = SystemTime::now();
        let since_the_epoch = start
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        self.ledger.store.online_weight().put(transaction.as_mut(), since_the_epoch.as_secs(), &self.online_m.lock().unwrap());
        let mut mutex = self.trended_m.lock().unwrap();
        *mutex = self.calculate_trend(transaction.txn());
    }

    pub fn trended(&self) -> Amount {
        self.trended_m.lock().unwrap().clone()
    }

    pub fn online(&self) -> Amount {
        let online = *self.online_m.lock().unwrap();
        online
    }

    pub fn delta(&self) -> Amount {
        let weight = max(self.online_m.lock().unwrap().clone(), self.trended_m.lock().unwrap().deref().clone());
        let weight = max(weight, self.node_config.online_weight_minimum);
        let amount = U256::from(weight.number()) * U256::from(ONLINE_WEIGHT_QUORUM) / U256::from(100);
        return Amount::new(amount.as_u128());
    }

    pub fn list(&self) -> Vec<Account> {
        self.reps.by_account.iter().map(|(a, b)| *a).collect()
    }

    pub fn clear(&mut self) {
        self.reps = EntryContainer::new();
        let mut mutex2 = self.online_m.lock().unwrap();
        *mutex2 = Amount::zero();
    }

    pub fn count(&self) -> usize {
        self.reps.len()
    }

    pub fn item_size() -> usize {
        size_of::<(usize, Entry)>()
    }
}

#[derive(Default)]
struct EntryContainer {
    entries: HashMap<usize, Entry>,
    by_account: HashMap<Account, usize>,
    by_time: BTreeMap<Instant, Vec<usize>>,
    next_id: usize,
}

impl EntryContainer {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn clear(&mut self) {
        self.entries = HashMap::new();
        self.by_account = HashMap::new();
        self.by_time = BTreeMap::new();
        self.next_id = 0;
    }

    pub fn insert(&mut self, entry: Entry) {
        let id = self.create_id();

        self.by_account.insert(entry.account, id);

        let by_time = self.by_time.entry(entry.time).or_default();
        by_time.push(id);

        self.entries.insert(id, entry);
        println!("REPS: {:?}", self.by_account);
    }

    fn create_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    pub fn by_account(&self, account: &Account) -> Option<usize> {
        match self.by_account.get(account) {
            Some(id) => Some(*id),
            None => None,
        }
    }

    fn trim(&mut self, upper_bound: Duration) -> bool {
        let mut trimmed = false;
        let mut instants_to_remove = Vec::new();
        for (&instant, ids) in self.by_time.iter() {
            if instant.elapsed() < upper_bound {
                break;
            }

            instants_to_remove.push(instant);

            for id in ids {
                let entry = self.entries.remove(id).unwrap();
                self.by_account.remove(&entry.account).unwrap();
                //let by_account = self.by_account.get_mut(&entry.account).unwrap();
                //by_account.remove(id);
                //if by_account.is_empty() {
                    //self.by_account.remove(&entry.account);
                //}
            }
            trimmed = true;
        }

        for instant in instants_to_remove {
            self.by_time.remove(&instant);
        }

        trimmed
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

struct Entry {
    account: Account,
    time: Instant,
}


