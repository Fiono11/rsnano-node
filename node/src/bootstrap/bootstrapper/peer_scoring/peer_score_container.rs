use super::peer_score::PeerScore;
use rsnano_network::ChannelId;
use rustc_hash::{FxHashMap, FxHashSet};

pub(super) struct PeerScoreContainer {
    scores: FxHashMap<ChannelId, PeerScore>,
    usable: FxHashSet<ChannelId>,
    channel_limit: usize,
}

impl PeerScoreContainer {
    pub const DEFAULT_CHANNEL_LIMIT: usize = 16;

    pub fn new(channel_limit: usize) -> Self {
        Self {
            scores: FxHashMap::default(),
            usable: FxHashSet::default(),
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

    pub fn insert(&mut self, channel_id: ChannelId, score: PeerScore) {
        self.scores.insert(channel_id, score);
        self.usable.insert(channel_id);
    }

    pub fn running_queries(&self, channel_id: ChannelId) -> usize {
        self.scores
            .get(&channel_id)
            .map(|p| p.running_queries)
            .unwrap_or_default()
    }

    pub fn request_sent(&mut self, channel_id: ChannelId) {
        let score = self.scores.entry(channel_id).or_default();
        score.request_sent();
        if score.running_queries >= self.channel_limit {
            self.usable.remove(&channel_id);
        }
    }

    pub fn got_response(&mut self, channel_id: ChannelId) -> bool {
        if let Some(score) = self.scores.get_mut(&channel_id) {
            score.got_response();
            if score.running_queries < self.channel_limit {
                self.usable.insert(channel_id);
            }
            true
        } else {
            false
        }
    }

    pub fn decay(&mut self) {
        for (channel_id, score) in self.scores.iter_mut() {
            score.decay();
            if score.running_queries < self.channel_limit {
                self.usable.insert(*channel_id);
            }
        }
    }

    pub fn remove(&mut self, channel_id: ChannelId) {
        self.scores.remove(&channel_id);
        self.usable.remove(&channel_id);
    }
}

impl Default for PeerScoreContainer {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CHANNEL_LIMIT)
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
        container.insert(channel_id, PeerScore::default());
        assert_eq!(container.len(), 1);
        assert!(container.get(channel_id).is_some());
        assert!(container.usable.contains(&channel_id));
    }

    #[test]
    fn remove() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        let another_channel_id = ChannelId::from(100);
        container.insert(channel_id, PeerScore::default());
        container.insert(another_channel_id, PeerScore::default());

        container.remove(channel_id);

        assert_eq!(container.len(), 1);
        assert!(container.get(channel_id).is_none());
        assert!(container.get(another_channel_id).is_some());
        assert!(!container.usable.contains(&channel_id));
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
        container.insert(channel_id, PeerScore::default());
        container.insert(another_channel_id, PeerScore::default());
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
        container.insert(channel_id, PeerScore::default());
        container.insert(another_channel_id, PeerScore::default());
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
        container.insert(channel_id, PeerScore::default());
        for _ in 0..PeerScoreContainer::DEFAULT_CHANNEL_LIMIT - 1 {
            container.request_sent(channel_id);
        }
        assert!(container.usable.contains(&channel_id));
        container.request_sent(channel_id);
        assert!(!container.usable.contains(&channel_id));
    }

    #[test]
    fn channel_becomes_usable_again_if_its_open_requests_fall_below_the_limit() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id, PeerScore::default());
        for _ in 0..PeerScoreContainer::DEFAULT_CHANNEL_LIMIT {
            container.request_sent(channel_id);
        }
        assert!(!container.usable.contains(&channel_id));
        container.got_response(channel_id);
        assert!(container.usable.contains(&channel_id));
    }

    #[test]
    fn channel_becomes_usable_again_if_its_open_requests_decay() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id, PeerScore::default());
        for _ in 0..PeerScoreContainer::DEFAULT_CHANNEL_LIMIT {
            container.request_sent(channel_id);
        }
        assert!(!container.usable.contains(&channel_id));
        container.decay();
        assert!(container.usable.contains(&channel_id));
    }
}
