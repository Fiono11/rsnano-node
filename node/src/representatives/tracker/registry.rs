use std::collections::VecDeque;

use rustc_hash::FxHashMap;

use rsnano_network::ChannelId;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::PublicKey;

#[derive(Clone)]
pub struct RegisteredRep {
    pub public_key: PublicKey,
    pub last_seen: Timestamp,
    pub channel_id: Option<ChannelId>,
}

impl RegisteredRep {
    pub fn new_test_instance() -> Self {
        Self {
            public_key: 1.into(),
            last_seen: Timestamp::new_test_instance(),
            channel_id: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RegisterResult {
    Inserted,
    Updated,
    ChannelChanged(ChannelId),
}

/// Collection of all representatives that are currently online.
/// It belongs to the logic layer.
#[derive(Default)]
pub(crate) struct RepresentativeRegistry {
    /// Insertion order, oldest first. `now` is non-decreasing, so this stays sorted by time.
    /// An entry is stale (and ignored) once `by_key` no longer agrees with its timestamp.
    order: VecDeque<(PublicKey, Timestamp)>,
    by_key: FxHashMap<PublicKey, RegisteredRep>,
    by_channel: FxHashMap<ChannelId, Vec<PublicKey>>,
    empty: Vec<PublicKey>,
}

impl RepresentativeRegistry {
    pub fn new() -> Self {
        Default::default()
    }

    #[cfg(test)]
    pub fn get(&self, key: &PublicKey) -> Option<&'_ RegisteredRep> {
        self.by_key.get(key)
    }

    pub fn reps_for_channel(&self, channel_id: ChannelId) -> impl Iterator<Item = &RegisteredRep> {
        self.by_channel
            .get(&channel_id)
            .unwrap_or(&self.empty)
            .iter()
            .filter_map(|k| self.by_key.get(k))
    }

    #[cfg(test)]
    pub fn contains_key(&self, key: &PublicKey) -> bool {
        self.by_key.contains_key(key)
    }

    pub fn contains_channel(&self, channel_id: ChannelId) -> bool {
        self.by_channel.contains_key(&channel_id)
    }

    pub fn peered_count(&self) -> usize {
        self.by_channel.values().map(|i| i.len()).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = &RegisteredRep> {
        self.by_key.values()
    }

    fn remove_key_from_channel(
        by_channel: &mut FxHashMap<ChannelId, Vec<PublicKey>>,
        channel_id: ChannelId,
        public_key: PublicKey,
    ) {
        if let Some(keys) = by_channel.get_mut(&channel_id) {
            if let Some(i) = keys.iter().position(|k| *k == public_key) {
                keys.swap_remove(i);
            }
            if keys.is_empty() {
                by_channel.remove(&channel_id);
            }
        }
    }

    /// Returns `true` if it was a new insert and `false` if an entry for that account was already present
    pub fn register(
        &mut self,
        public_key: PublicKey,
        channel_id: Option<ChannelId>,
        now: Timestamp,
    ) -> RegisterResult {
        self.order.push_back((public_key, now));
        let by_key = &mut self.by_key;
        let by_channel = &mut self.by_channel;

        if let Some(entry) = by_key.get_mut(&public_key) {
            entry.last_seen = now;

            if let Some(id) = channel_id
                && entry.channel_id != channel_id
            {
                if let Some(old_id) = entry.channel_id {
                    Self::remove_key_from_channel(by_channel, old_id, public_key);
                }

                by_channel.entry(id).or_default().push(public_key);

                entry.channel_id = channel_id;
                RegisterResult::ChannelChanged(id)
            } else {
                RegisterResult::Updated
            }
        } else {
            by_key.insert(
                public_key,
                RegisteredRep {
                    public_key,
                    last_seen: now,
                    channel_id,
                },
            );
            if let Some(id) = channel_id {
                by_channel.entry(id).or_default().push(public_key);
            }
            RegisterResult::Inserted
        }
    }

    pub fn disconnected(&mut self, channel_id: ChannelId) -> Vec<PublicKey> {
        let Some(keys) = self.by_channel.remove(&channel_id) else {
            return Vec::new();
        };
        for key in &keys {
            if let Some(rep) = self.by_key.get_mut(key) {
                rep.channel_id = None;
            }
        }
        keys
    }

    pub fn trim(&mut self, upper_bound: Timestamp) -> Vec<(PublicKey, Timestamp)> {
        let mut trimmed = Vec::new();

        while let Some((_, time)) = self.order.front() {
            if *time >= upper_bound {
                break;
            }

            let (public_key, time) = self.order.pop_front().unwrap();
            // Only the entry matching the current timestamp in `by_account` is canonical;
            // older entries left behind by an update are stale and get discarded here.
            if let Some(entry) = self.by_key.get(&public_key)
                && entry.last_seen == time
            {
                let channel_id = entry.channel_id;
                self.by_key.remove(&public_key);
                if let Some(id) = channel_id {
                    Self::remove_key_from_channel(&mut self.by_channel, id, public_key);
                }
                trimmed.push((public_key, time));
            }
        }

        trimmed
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }
}

impl<'a> IntoIterator for &'a RepresentativeRegistry {
    type Item = &'a RegisteredRep;

    type IntoIter = std::collections::hash_map::Values<'a, PublicKey, RegisteredRep>;

    fn into_iter(self) -> Self::IntoIter {
        self.by_key.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty() {
        let registry = RepresentativeRegistry::new();
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.iter().count(), 0);
        assert_eq!(registry.peered_count(), 0);
    }

    /*
     * Register
     */

    #[test]
    fn registers_a_new_rep() {
        let mut registry = RepresentativeRegistry::new();

        let now = Timestamp::new_test_instance();
        let result = registry.register(PublicKey::from(1), None, now);

        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.iter().next().unwrap().public_key,
            PublicKey::from(1)
        );
        assert_eq!(result, RegisterResult::Inserted);
        assert_eq!(registry.peered_count(), 0);
    }

    #[test]
    fn registers_multiple_reps() {
        let mut registry = RepresentativeRegistry::new();

        let now = Timestamp::new_test_instance();
        let new_insert_a = registry.register(PublicKey::from(1), None, now);
        let new_insert_b =
            registry.register(PublicKey::from(2), None, now + Duration::from_secs(1));

        assert_eq!(registry.len(), 2);
        assert_eq!(new_insert_a, RegisterResult::Inserted);
        assert_eq!(new_insert_b, RegisterResult::Inserted);
    }

    #[test]
    fn register_updates_last_seen_time() {
        let mut registry = RepresentativeRegistry::new();

        let now = Timestamp::new_test_instance();
        let new_insert_a = registry.register(PublicKey::from(1), None, now);
        let new_insert_b =
            registry.register(PublicKey::from(1), None, now + Duration::from_secs(1));

        assert_eq!(registry.len(), 1);
        assert_eq!(new_insert_a, RegisterResult::Inserted);
        assert_eq!(new_insert_b, RegisterResult::Updated);
        // The stale entry from the first insert is still queued and gets
        // discarded lazily once it reaches the front during a trim.
        assert_eq!(registry.order.len(), 2);
    }

    #[test]
    fn keeps_track_of_channel_id() {
        let mut registry = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        let key = PublicKey::from(1);
        let channel_id = ChannelId::from(42);
        assert!(!registry.contains_channel(channel_id));

        registry.register(key, Some(channel_id), now);

        assert_eq!(registry.get(&key).unwrap().channel_id, Some(channel_id),);
        assert!(registry.contains_channel(channel_id));
    }

    #[test]
    fn omitted_channel_id_leaves_the_registered_channel_unchanged() {
        let mut registry = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        let key = PublicKey::from(1);
        let channel_id = ChannelId::from(42);

        registry.register(key, Some(channel_id), now);
        registry.register(key, None, now);

        assert_eq!(registry.get(&key).unwrap().channel_id, Some(channel_id),);
        assert!(registry.contains_channel(channel_id));
    }

    #[test]
    fn tracks_channel_changes() {
        let mut registry = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        let key = PublicKey::from(1);
        let channel_id = ChannelId::from(42);
        let new_channel_id = ChannelId::from(1000);

        registry.register(key, Some(channel_id), now);
        let result = registry.register(key, Some(new_channel_id), now);

        assert_eq!(result, RegisterResult::ChannelChanged(new_channel_id));
        assert_eq!(registry.get(&key).unwrap().channel_id, Some(new_channel_id),);
        assert!(!registry.contains_channel(channel_id));
        assert!(registry.contains_channel(new_channel_id));
        assert_eq!(registry.peered_count(), 1);
    }

    #[test]
    fn removes_disconnected_channel() {
        let mut registry = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        let key = PublicKey::from(1);
        let channel_id = ChannelId::from(42);
        registry.register(key, Some(channel_id), now);

        registry.disconnected(channel_id);

        assert!(!registry.contains_channel(channel_id));
        assert!(registry.contains_key(&key));
    }

    /*
     * Trimming
     */

    #[test]
    fn trimming_empty_registry_does_nothing() {
        let mut registry = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        assert_eq!(registry.trim(now).len(), 0);
    }

    #[test]
    fn dont_trim_if_upper_bound_not_reached() {
        let mut registry = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        registry.register(PublicKey::from(1), None, now);
        assert_eq!(registry.trim(now).len(), 0);
    }

    #[test]
    fn trim_if_upper_bound_reached() {
        let mut registry = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        registry.register(PublicKey::from(1), None, now);
        assert_eq!(registry.trim(now + Duration::from_millis(1)).len(), 1);
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn trim_removes_only_the_expired_rep_from_channel() {
        let mut registry = RepresentativeRegistry::new();
        let now = Timestamp::new_test_instance();
        let key1 = PublicKey::from(1);
        let key2 = PublicKey::from(2);
        let channel_id = ChannelId::from(42);

        registry.register(key1, Some(channel_id), now);
        registry.register(key2, Some(channel_id), now + Duration::from_secs(10));

        let trimmed = registry.trim(now + Duration::from_millis(1));

        assert_eq!(trimmed.len(), 1);
        assert_eq!(trimmed[0].0, key1);
        assert_eq!(registry.len(), 1);
        // Channel must still exist because key2 is still registered on it
        assert!(registry.contains_channel(channel_id));
        assert_eq!(registry.reps_for_channel(channel_id).count(), 1);
    }

    #[test]
    fn trim_multiple_entries() {
        let mut registry = RepresentativeRegistry::new();

        let now = Timestamp::new_test_instance();
        registry.register(PublicKey::from(1), None, now);
        registry.register(PublicKey::from(2), None, now);
        registry.register(PublicKey::from(3), None, now + Duration::from_secs(1));
        registry.register(PublicKey::from(4), None, now + Duration::from_secs(2));

        assert_eq!(registry.trim(now + Duration::from_millis(1500)).len(), 3);
        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.iter().next().unwrap().public_key,
            PublicKey::from(4)
        );
        assert_eq!(registry.order.len(), 1);
    }
}
