use super::peer_score::PeerScore;
use rsnano_network::ChannelId;
use rustc_hash::{FxHashMap, FxHashSet};

pub(super) struct PeerScoreContainer {
    scores: FxHashMap<ChannelId, PeerScore>,
    usable: UsableChannels,
    channel_limit: usize,
}

impl PeerScoreContainer {
    pub const DEFAULT_CHANNEL_LIMIT: usize = 16;

    pub fn new(channel_limit: usize) -> Self {
        Self {
            scores: FxHashMap::default(),
            usable: UsableChannels::default(),
            channel_limit,
        }
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    #[cfg(test)]
    pub fn get(&self, channel_id: ChannelId) -> Option<&PeerScore> {
        self.scores.get(&channel_id)
    }

    pub fn insert(&mut self, channel_id: ChannelId) {
        self.scores
            .insert(channel_id, PeerScore::new(self.channel_limit));
        self.usable.insert(channel_id);
    }

    pub fn usable(&self) -> &[ChannelId] {
        self.usable.as_slice()
    }

    fn change_score<F>(&mut self, channel_id: ChannelId, f: F) -> bool
    where
        F: FnOnce(&mut PeerScore),
    {
        let Some(score) = self.scores.get_mut(&channel_id) else {
            return false;
        };
        f(score);
        if score.is_good() {
            self.usable.insert(channel_id);
        } else {
            self.usable.remove(channel_id);
        }
        true
    }

    pub fn channel_full(&mut self, channel_id: ChannelId) {
        self.change_score(channel_id, |score| {
            score.priority_down(16.0);
            score.channel_full += 1;
        });
    }

    pub fn request_sent(&mut self, channel_id: ChannelId) {
        self.change_score(channel_id, |score| {
            score.request_sent();
            score.priority_down(1.0);
        });
    }

    pub fn got_response(&mut self, channel_id: ChannelId) -> bool {
        self.change_score(channel_id, |score| {
            score.got_response();
            score.priority_up(1.0);
        })
    }

    pub fn blocks_received(&mut self, channel_id: ChannelId) {
        self.change_score(channel_id, |score| {
            score.priority_up(1.0);
            score.blocks_received += 1;
        });
    }

    pub fn out_of_date(&mut self, channel_id: ChannelId) {
        self.change_score(channel_id, |score| {
            score.priority_down(64.0);
            score.out_of_date += 1;
        });
    }

    pub fn decay(&mut self) {
        for (channel_id, score) in self.scores.iter_mut() {
            score.decay();
            if score.is_good() {
                self.usable.insert(*channel_id);
            }
        }
    }

    pub fn timed_out(&mut self, channel_id: ChannelId) {
        self.change_score(channel_id, |score| {
            score.remove_query();
            score.priority_down(16.0);
            score.timeouts += 1;
        });
    }

    pub fn remove(&mut self, channel_id: ChannelId) {
        self.scores.remove(&channel_id);
        self.usable.remove(channel_id);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ChannelId, &PeerScore)> {
        self.scores.iter()
    }
}

impl Default for PeerScoreContainer {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CHANNEL_LIMIT)
    }
}

#[derive(Default)]
struct UsableChannels {
    usable: FxHashSet<ChannelId>,
    usable_vec: Vec<ChannelId>,
}

impl UsableChannels {
    pub fn insert(&mut self, channel_id: ChannelId) {
        if self.usable.insert(channel_id) {
            self.usable_vec.push(channel_id);
        }
    }

    fn remove(&mut self, channel_id: ChannelId) {
        if self.usable.remove(&channel_id) {
            if let Some(index) = self.usable_vec.iter().position(|id| *id == channel_id) {
                self.usable_vec.swap_remove(index);
            }
        }
    }

    pub fn as_slice(&self) -> &[ChannelId] {
        &self.usable_vec
    }

    #[allow(dead_code)]
    pub fn contains(&self, channel_id: ChannelId) -> bool {
        self.usable.contains(&channel_id)
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.usable_vec.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let container = PeerScoreContainer::default();
        assert_eq!(container.len(), 0);
        assert_eq!(container.usable.len(), 0);
    }

    #[test]
    fn insert() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id);
        assert_eq!(container.len(), 1);
        assert!(container.get(channel_id).is_some());
        assert!(container.usable.contains(channel_id));
    }

    #[test]
    fn remove() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        let another_channel_id = ChannelId::from(100);
        container.insert(channel_id);
        container.insert(another_channel_id);

        container.remove(channel_id);

        assert_eq!(container.len(), 1);
        assert!(container.get(channel_id).is_none());
        assert!(container.get(another_channel_id).is_some());
        assert!(!container.usable.contains(channel_id));
    }

    #[test]
    fn remove_non_existing() {
        let mut container = PeerScoreContainer::default();
        container.remove(ChannelId::from(42));
        assert_eq!(container.len(), 0);
    }

    #[test]
    fn got_response_is_noop_if_channel_unknown() {
        let mut container = PeerScoreContainer::default();
        let modified = container.got_response(ChannelId::from(42));
        assert!(!modified);
        assert_eq!(container.len(), 0);
    }

    #[test]
    fn got_response() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        let another_channel_id = ChannelId::from(100);
        container.insert(channel_id);
        container.insert(another_channel_id);
        container.request_sent(channel_id);
        container.request_sent(channel_id);
        let modified = container.got_response(channel_id);
        assert!(modified);
        assert_eq!(container.get(channel_id).unwrap().running_queries, 1);
        assert_eq!(
            container.get(another_channel_id).unwrap().running_queries,
            0
        );
    }

    #[test]
    fn decay() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        let another_channel_id = ChannelId::from(100);
        container.insert(channel_id);
        container.insert(another_channel_id);
        container.request_sent(channel_id);
        container.request_sent(channel_id);
        container.request_sent(another_channel_id);

        container.decay();

        assert_eq!(container.get(channel_id).unwrap().running_queries, 1);
        assert_eq!(
            container.get(another_channel_id).unwrap().running_queries,
            0
        );
    }

    #[test]
    fn channel_becomes_unusable_if_it_has_too_many_open_requests() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id);
        for _ in 0..PeerScoreContainer::DEFAULT_CHANNEL_LIMIT - 1 {
            container.request_sent(channel_id);
        }
        assert!(container.usable.contains(channel_id));
        container.request_sent(channel_id);
        assert!(!container.usable.contains(channel_id));
    }

    #[test]
    fn channel_becomes_usable_again_if_its_open_requests_fall_below_the_limit() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id);
        for _ in 0..PeerScoreContainer::DEFAULT_CHANNEL_LIMIT {
            container.request_sent(channel_id);
        }
        assert!(!container.usable.contains(channel_id));
        container.got_response(channel_id);
        assert!(container.usable.contains(channel_id));
    }

    #[test]
    fn channel_becomes_usable_again_if_its_open_requests_decay() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id);
        for _ in 0..PeerScoreContainer::DEFAULT_CHANNEL_LIMIT {
            container.request_sent(channel_id);
        }
        assert!(!container.usable.contains(channel_id));
        container.decay();
        assert!(container.usable.contains(channel_id));
    }
}
