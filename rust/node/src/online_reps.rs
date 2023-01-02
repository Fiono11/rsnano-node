use std::cmp::max;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use rsnano_core::{Account, Amount, BlockHash, Root};
use rsnano_ledger::Ledger;
use rsnano_store_traits::{ReadTransaction, Transaction, WriteTransaction};
use crate::config::NodeConfig;
use crate::stats::DetailType::Send;
use crate::voting::VoteSpacing;

pub const ONLINE_WEIGHT_QUORUM: u128 = 67;

pub struct OnlineReps {
    pub ledger: Arc<Ledger>,
    pub node_config: Arc<NodeConfig>,
    reps: EntryContainer,
    trended_m: Arc<Mutex<Amount>>,
    online_m: Arc<Mutex<Amount>>,
    minimum: Arc<Mutex<Amount>>,
}

impl OnlineReps {
    pub fn new(ledger: Arc<Ledger>, node_config: Arc<NodeConfig>) -> Self {

        let transaction = ledger.store.tx_begin_read().unwrap();
        let trended_m = Arc::new(Mutex::new(Self::calculate_trend(transaction.txn(), &ledger, &node_config)));

        Self {
            ledger,
            node_config,
            reps: EntryContainer::new(),
            trended_m,
            online_m: Arc::new(Mutex::new(Amount::zero())),
            minimum: Arc::new(Mutex::new(Amount::zero())),
        }
    }

    pub fn calculate_online(&self) -> Amount {
        let mut current = Amount::zero();
        for i in 0..self.reps.entries.len() {
            current += self.ledger.weight(&self.reps.entries.get(&i).unwrap().account);
        }
        current
    }

    pub fn calculate_trend(transaction_a: &dyn Transaction, ledger: &Arc<Ledger>, node_config: &NodeConfig) -> Amount {
        let mut items = Vec::new();
        items.push(node_config.online_weight_minimum);
        let mut it = ledger.store.online_weight().begin(transaction_a);
        while !it.is_end() {
            items.push(*it.current().unwrap().1);
            it.next();
        }
        let median_idx = items.len() / 2;
        items[median_idx]
    }

    pub fn observe(&mut self, rep_a: Account) {
        if self.ledger.weight(&rep_a).number() > 0 {
            let new_insert: bool = self.reps.by_account.remove(&rep_a).is_some();
            let time = Instant::now();
            let entry = Entry {
                account: rep_a,
                time,
            };
            self.reps.insert(entry);
            let cutoff = time; //- Duration::from_secs(self.node_config.network_params.node.weight_period);
            let trimmed = self.reps.by_time.first_key_value().unwrap().0 != &cutoff;
            self.reps.trim(cutoff.elapsed());
            if new_insert || trimmed {
                self.online_m = Arc::new(Mutex::new(self.calculate_online()));
            }
        }
    }

    pub fn sample(&mut self) {
        let mut transaction = self.ledger.store.tx_begin_write().unwrap();
        while self.ledger.store.online_weight().count(transaction.txn()) >= 10 {//self.node_config.network_params.node.max_weight_samples {
            let oldest = self.ledger.store.online_weight().begin(transaction.txn());
            //debug_assert!(oldest != self.ledger.store.online_weight().rbegin(transaction.txn()));
            self.ledger.store.online_weight().del(transaction.as_mut(), *oldest.current().unwrap().0);
        }
        self.ledger.store.online_weight().put(transaction.as_mut(), Instant::now().elapsed().as_secs(), &self.online_m.lock().unwrap());
        self.trended_m = Arc::new(Mutex::new(Self::calculate_trend(transaction.txn(), &self.ledger, &self.node_config)));
    }

    pub fn trended(&self) -> Amount {
        self.trended_m.lock().unwrap().deref().clone()
    }

    pub fn online(&self) -> Amount {
        self.online_m.lock().unwrap().deref().clone()
    }

    pub fn delta(&self) -> Amount {
        let weight = max(self.online_m.lock().unwrap().deref().clone(), self.trended_m.lock().unwrap().deref().clone());
        let weight = max(weight, self.node_config.online_weight_minimum);
        return Amount::new(weight.number() * ONLINE_WEIGHT_QUORUM / 100);
    }

    pub fn list(&self) -> Vec<Account> {
        self.reps.by_account.iter().map(|(a, b)| *a).collect()
    }

    pub fn clear(&mut self) {
        self.reps = EntryContainer::new();
        self.online_m = Arc::new(Mutex::new(Amount::zero()));
    }
}

#[derive(Default)]
struct EntryContainer {
    entries: HashMap<usize, Entry>,
    by_account: HashMap<Account, HashSet<usize>>,
    by_time: BTreeMap<Instant, Vec<usize>>,
    next_id: usize,
    empty_id_set: HashSet<usize>,
}

impl EntryContainer {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn insert(&mut self, entry: Entry) {
        let id = self.create_id();

        let by_account = self.by_account.entry(entry.account).or_default();
        by_account.insert(id);

        let by_time = self.by_time.entry(entry.time).or_default();
        by_time.push(id);

        self.entries.insert(id, entry);
    }

    fn create_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    pub fn by_account(&self, account: &Account) -> impl Iterator<Item = &Entry> + '_ {
        match self.by_account.get(account) {
            Some(ids) => self.iter_entries(ids),
            None => self.iter_entries(&self.empty_id_set),
        }
    }

    fn iter_entries<'a>(&'a self, ids: &'a HashSet<usize>) -> impl Iterator<Item = &Entry> + 'a {
        ids.iter().map(|&id| &self.entries[&id])
    }

    fn trim(&mut self, upper_bound: Duration) {
        let mut instants_to_remove = Vec::new();
        for (&instant, ids) in self.by_time.iter() {
            if instant.elapsed() < upper_bound {
                break;
            }

            instants_to_remove.push(instant);

            for id in ids {
                let entry = self.entries.remove(id).unwrap();

                let by_account = self.by_account.get_mut(&entry.account).unwrap();
                by_account.remove(id);
                if by_account.is_empty() {
                    self.by_account.remove(&entry.account);
                }
            }
        }

        for instant in instants_to_remove {
            self.by_time.remove(&instant);
        }
    }

    fn change_time_for_account(&mut self, account: &Account, time: Instant) -> bool {
        match self.by_account.get(account) {
            Some(ids) => {
                change_time_for_entries(ids, time, &mut self.entries, &mut self.by_time);
                true
            }
            None => false,
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

struct Entry {
    account: Account,
    time: Instant,
}

fn change_time_for_entries(
    ids: &HashSet<usize>,
    time: Instant,
    entries: &mut HashMap<usize, Entry>,
    by_time: &mut BTreeMap<Instant, Vec<usize>>,
) {
    for id in ids {
        change_time_for_entry(id, time, entries, by_time);
    }
}

fn change_time_for_entry(
    id: &usize,
    time: Instant,
    entries: &mut HashMap<usize, Entry>,
    by_time: &mut BTreeMap<Instant, Vec<usize>>,
) {
    if let Some(entry) = entries.get_mut(id) {
        let old_time = entry.time;
        entry.time = time;
        remove_from_time_index(old_time, id, by_time);
        by_time.entry(time).or_default().push(*id);
    }
}

fn remove_from_time_index(
    time: Instant,
    id: &usize,
    ids_by_time: &mut BTreeMap<Instant, Vec<usize>>,
) {
    if let Some(ids) = ids_by_time.get_mut(&time) {
        if ids.len() == 1 {
            ids_by_time.remove(&time);
        } else {
            ids.retain(|x| x != id);
        }
    }
}

