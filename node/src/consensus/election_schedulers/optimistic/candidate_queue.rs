use std::collections::{HashMap, VecDeque};

use rsnano_nullable_clock::Timestamp;
use rsnano_types::Account;

#[derive(Default)]
pub(super) struct CandidateQueue {
    by_account: HashMap<Account, Timestamp>,
    sequenced: VecDeque<Account>,
}

impl CandidateQueue {
    pub fn insert(&mut self, account: Account, time: Timestamp, gap: u64) {
        if self.by_account.insert(account, time).is_some() {
            self.sequenced.retain(|i| *i != account);
        }
        self.sequenced.push_back(account);
    }

    pub fn len(&self) -> usize {
        self.sequenced.len()
    }

    pub fn contains(&self, account: &Account) -> bool {
        self.by_account.contains_key(account)
    }

    pub fn front(&self) -> Option<(Account, Timestamp)> {
        self.sequenced
            .front()
            .and_then(|account| self.by_account.get(account).map(|time| (*account, *time)))
    }

    pub fn pop_front(&mut self) -> Option<(Account, Timestamp)> {
        self.sequenced.pop_front().map(|account| {
            let time = self.by_account.remove(&account).unwrap();
            (account, time)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_queue_has_zero_len() {
        let q = CandidateQueue::default();
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn insert_increases_len() {
        let mut q = CandidateQueue::default();
        q.insert(Account::from(1), Timestamp::new_test_instance(), 1);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn contains_returns_true_after_insert() {
        let mut q = CandidateQueue::default();
        let account = Account::from(1);
        q.insert(account, Timestamp::new_test_instance(), 1);
        assert!(q.contains(&account));
    }

    #[test]
    fn contains_returns_false_for_unknown_account() {
        let q = CandidateQueue::default();
        assert!(!q.contains(&Account::from(1)));
    }

    #[test]
    fn front_returns_none_when_empty() {
        let q = CandidateQueue::default();
        assert!(q.front().is_none());
    }

    #[test]
    fn front_returns_oldest_entry() {
        let mut q = CandidateQueue::default();
        let a = Account::from(1);
        let b = Account::from(2);
        q.insert(a, Timestamp::new_test_instance(), 1);
        q.insert(b, Timestamp::new_test_instance(), 1);
        let (account, _) = q.front().unwrap();
        assert_eq!(account, a);
    }

    #[test]
    fn pop_front_returns_in_insertion_order() {
        let mut q = CandidateQueue::default();
        let a = Account::from(1);
        let b = Account::from(2);
        q.insert(a, Timestamp::new_test_instance(), 1);
        q.insert(b, Timestamp::new_test_instance(), 1);

        let (first, _) = q.pop_front().unwrap();
        let (second, _) = q.pop_front().unwrap();
        assert_eq!(first, a);
        assert_eq!(second, b);
    }

    #[test]
    fn pop_front_removes_entry() {
        let mut q = CandidateQueue::default();
        let account = Account::from(1);
        q.insert(account, Timestamp::new_test_instance(), 1);
        q.pop_front();
        assert!(!q.contains(&account));
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn insert_duplicate_moves_to_back() {
        let mut q = CandidateQueue::default();
        let a = Account::from(1);
        let b = Account::from(2);
        q.insert(a, Timestamp::new_test_instance(), 1);
        q.insert(b, Timestamp::new_test_instance(), 1);
        q.insert(a, Timestamp::new_test_instance(), 1); // re-insert a — should move to back

        let (first, _) = q.pop_front().unwrap();
        assert_eq!(first, b);
        let (second, _) = q.pop_front().unwrap();
        assert_eq!(second, a);
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn pop_front_returns_none_when_empty() {
        let mut q = CandidateQueue::default();
        assert!(q.pop_front().is_none());
    }
}
