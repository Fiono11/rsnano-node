use super::ordered_priorities::PriorityEntry;
use ordered_float::OrderedFloat;
use rsnano_core::{Account, BlockHash};
use std::{
    collections::{BTreeMap, VecDeque},
    mem::size_of,
};

pub(crate) struct BlockingEntry {
    pub account: Account,
    pub dependency: BlockHash,
    pub original_entry: PriorityEntry,
}

impl BlockingEntry {
    fn priority(&self) -> OrderedFloat<f32> {
        self.original_entry.priority
    }
}

/// A blocked account is an account that has failed to insert a new block because the source block is not currently present in the ledger
/// An account is unblocked once it has a block successfully inserted
#[derive(Default)]
pub(crate) struct OrderedBlocking {
    by_account: BTreeMap<Account, BlockingEntry>,
    sequenced: VecDeque<Account>,
    by_priority: BTreeMap<OrderedFloat<f32>, VecDeque<Account>>,
}

impl OrderedBlocking {
    pub const ELEMENT_SIZE: usize =
        size_of::<BlockingEntry>() + size_of::<Account>() * 3 + size_of::<f32>();

    pub fn len(&self) -> usize {
        self.sequenced.len()
    }

    pub fn insert(&mut self, entry: BlockingEntry) -> bool {
        let account = entry.account;
        let prio = entry.priority();
        if self.by_account.contains_key(&account) {
            return false;
        }

        self.by_account.insert(account, entry);
        self.sequenced.push_back(account);
        self.by_priority.entry(prio).or_default().push_back(account);
        true
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    pub fn get(&self, account: &Account) -> Option<&BlockingEntry> {
        self.by_account.get(account)
    }

    pub fn remove(&mut self, account: &Account) {
        if let Some(entry) = self.by_account.remove(account) {
            self.sequenced.retain(|i| i != account);
            let accounts = self.by_priority.get_mut(&entry.priority()).unwrap();
            if accounts.len() > 1 {
                accounts.retain(|i| i != account);
            } else {
                self.by_priority.remove(&entry.priority());
            }
        }
    }

    pub fn pop_lowest_priority(&mut self) -> Option<BlockingEntry> {
        if let Some(mut entry) = self.by_priority.first_entry() {
            let accounts = entry.get_mut();
            let account = accounts[0];
            if accounts.len() > 1 {
                accounts.pop_front();
            } else {
                entry.remove();
            }
            self.sequenced.retain(|i| *i != account);
            Some(self.by_account.remove(&account).unwrap())
        } else {
            None
        }
    }
}