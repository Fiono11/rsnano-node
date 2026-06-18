use std::{collections::VecDeque, mem::size_of};

use rustc_hash::FxHashMap;

use rsnano_nullable_clock::Timestamp;
use rsnano_types::PublicKey;

/// Collection of all representatives that are currently online
#[derive(Default)]
pub(super) struct RepresentativeRegistry {
    /// Insertion order, oldest first. `now` is non-decreasing, so this stays sorted by time.
    /// An entry is stale (and ignored) once `by_account` no longer agrees with its timestamp.
    order: VecDeque<(PublicKey, Timestamp)>,
    by_account: FxHashMap<PublicKey, Timestamp>,
}

impl RepresentativeRegistry {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PublicKey> {
        self.by_account.keys()
    }

    /// Returns `true` if it was a new insert and `false` if an entry for that account was already present
    pub fn insert(&mut self, rep: PublicKey, now: Timestamp) -> bool {
        self.order.push_back((rep, now));

        if let Some(time) = self.by_account.get_mut(&rep) {
            *time = now;
            false // not inserted, just updated
        } else {
            self.by_account.insert(rep, now);
            true // inserted
        }
    }

    pub fn trim(&mut self, upper_bound: Timestamp) -> Vec<(PublicKey, Timestamp)> {
        let mut trimmed = Vec::new();

        while let Some((_, time)) = self.order.front() {
            if *time >= upper_bound {
                break;
            }

            let (account, time) = self.order.pop_front().unwrap();
            // Only the entry matching the current timestamp in `by_account` is canonical;
            // older entries left behind by an update are stale and get discarded here.
            if self.by_account.get(&account) == Some(&time) {
                self.by_account.remove(&account);
                trimmed.push((account, time));
            }
        }

        trimmed
    }

    pub fn len(&self) -> usize {
        self.by_account.len()
    }

    pub const ELEMENT_SIZE: usize =
        size_of::<(PublicKey, Timestamp)>() + size_of::<(PublicKey, Timestamp)>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty_container() {
        let container = RepresentativeRegistry::new();
        assert_eq!(container.len(), 0);
        assert_eq!(container.iter().count(), 0);
    }

    #[test]
    fn insert_one_rep() {
        let mut container = RepresentativeRegistry::new();

        let new_insert = container.insert(PublicKey::from(1), Timestamp::new_test_instance());

        assert_eq!(container.len(), 1);
        assert_eq!(container.iter().count(), 1);
        assert_eq!(container.iter().next().unwrap(), &PublicKey::from(1));
        assert_eq!(new_insert, true);
    }

    #[test]
    fn insert_two_reps() {
        let mut container = RepresentativeRegistry::new();

        let now = Timestamp::new_test_instance();
        let new_insert_a = container.insert(PublicKey::from(1), now);
        let new_insert_b = container.insert(PublicKey::from(2), now + Duration::from_secs(1));

        assert_eq!(container.len(), 2);
        assert_eq!(container.iter().count(), 2);
        assert_eq!(new_insert_a, true);
        assert_eq!(new_insert_b, true);
    }

    #[test]
    fn insert_same_rep_twice_with_same_time() {
        let mut container = RepresentativeRegistry::new();

        let now = Timestamp::new_test_instance();
        let new_insert_a = container.insert(PublicKey::from(1), now);
        let new_insert_b = container.insert(PublicKey::from(1), now);

        assert_eq!(container.len(), 1);
        assert_eq!(container.iter().count(), 1);
        assert_eq!(new_insert_a, true);
        assert_eq!(new_insert_b, false);
    }

    #[test]
    fn insert_same_rep_twice_with_different_time() {
        let mut container = RepresentativeRegistry::new();

        let now = Timestamp::new_test_instance();
        let new_insert_a = container.insert(PublicKey::from(1), now);
        let new_insert_b = container.insert(PublicKey::from(1), now + Duration::from_secs(1));

        assert_eq!(container.len(), 1);
        assert_eq!(container.iter().count(), 1);
        assert_eq!(new_insert_a, true);
        assert_eq!(new_insert_b, false);
        // The stale entry from the first insert is still queued and gets
        // discarded lazily once it reaches the front during a trim.
        assert_eq!(container.order.len(), 2);
    }

    #[test]
    fn trimming_empty_container_does_nothing() {
        let mut container = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        assert_eq!(container.trim(now).len(), 0);
    }

    #[test]
    fn dont_trim_if_upper_bound_not_reached() {
        let mut container = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        container.insert(PublicKey::from(1), now);
        assert_eq!(container.trim(now).len(), 0);
    }

    #[test]
    fn trim_if_upper_bound_reached() {
        let mut container = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        container.insert(PublicKey::from(1), now);
        assert_eq!(container.trim(now + Duration::from_millis(1)).len(), 1);
        assert_eq!(container.len(), 0);
    }

    #[test]
    fn trim_multiple_entries() {
        let mut container = RepresentativeRegistry::new();

        let now = Timestamp::new_test_instance();
        container.insert(PublicKey::from(1), now);
        container.insert(PublicKey::from(2), now);
        container.insert(PublicKey::from(3), now + Duration::from_secs(1));
        container.insert(PublicKey::from(4), now + Duration::from_secs(2));

        assert_eq!(container.trim(now + Duration::from_millis(1500)).len(), 3);
        assert_eq!(container.len(), 1);
        assert_eq!(container.iter().next().unwrap(), &PublicKey::from(4));
        assert_eq!(container.order.len(), 1);
    }
}
