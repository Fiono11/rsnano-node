use std::collections::{BTreeMap, HashMap};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::Account;

#[derive(Default)]
pub(super) struct CandidateQueue {
    /// account => gap
    by_account: HashMap<Account, u64>,
    /// gap => (account, insertion timestamp)
    by_gap: BTreeMap<u64, Vec<(Account, Timestamp)>>,
}

impl CandidateQueue {
    pub fn insert(&mut self, account: Account, now: Timestamp, gap: u64) {
        if self.by_account.insert(account, gap).is_some() {
            // Skip, because it is already in the queue
            return;
        }
        self.by_gap.entry(gap).or_default().push((account, now));
    }

    pub fn len(&self) -> usize {
        self.by_account.len()
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    pub fn has_candidate(&self, cutoff: Timestamp) -> bool {
        self.by_gap
            .values()
            .rev()
            .any(|i| i.iter().any(|(_, inserted)| *inserted <= cutoff))
    }

    pub fn pop_first(&mut self, cutoff: Timestamp) -> Option<Account> {
        let mut to_remove: Option<u64> = None;
        let mut result = None;
        for (gap, v) in self.by_gap.iter_mut().rev() {
            if let Some((account, _)) = v.pop_if(|(_, inserted)| *inserted <= cutoff) {
                if v.is_empty() {
                    to_remove = Some(*gap);
                }
                result = Some(account);
                break;
            }
        }

        if let Some(gap) = to_remove {
            self.by_gap.remove(&gap);
        }
        if let Some(account) = &result {
            self.by_account.remove(account);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let mut q = CandidateQueue::default();
        assert_eq!(q.len(), 0);
        assert!(!q.has_candidate(now()));
        assert!(q.pop_first(now()).is_none());
    }

    #[test]
    fn insert() {
        let mut q = CandidateQueue::default();
        q.insert(Account::from(1), now(), 1);
        assert_eq!(q.len(), 1);
        assert_eq!(q.by_account.len(), 1);
        assert_eq!(q.by_gap.len(), 1);
    }

    #[test]
    fn contains() {
        let mut q = CandidateQueue::default();
        let account = Account::from(1);

        assert!(!q.contains(&account));

        q.insert(account, now(), 1);
        assert!(q.contains(&account));
    }

    #[test]
    fn pop_first_returns_highest_gap_first() {
        let mut q = CandidateQueue::default();
        let a = Account::from(1);
        let b = Account::from(2);
        q.insert(a, now(), 2);
        q.insert(b, now(), 1);
        let account = q.pop_first(now()).unwrap();
        assert_eq!(account, a);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn pop_first_orders_by_gap_descending() {
        let mut q = CandidateQueue::default();
        let a = Account::from(1);
        let b = Account::from(2);
        q.insert(a, now(), 2);
        q.insert(b, now(), 1);

        let first = q.pop_first(now()).unwrap();
        let second = q.pop_first(now()).unwrap();
        assert_eq!(first, a);
        assert_eq!(second, b);
    }

    #[test]
    fn insert_duplicate_changes_nothing() {
        let mut q = CandidateQueue::default();
        let a = Account::from(1);
        let b = Account::from(2);
        q.insert(a, now(), 2);
        q.insert(b, now(), 1);
        q.insert(a, now(), 2); // re-insert

        let first = q.pop_first(now()).unwrap();
        assert_eq!(first, a);
        let second = q.pop_first(now()).unwrap();
        assert_eq!(second, b);
        assert_eq!(q.len(), 0);
    }

    fn now() -> Timestamp {
        Timestamp::new_test_instance()
    }
}
