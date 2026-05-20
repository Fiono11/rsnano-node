use super::peer_score::PeerScore;
use rsnano_network::ChannelId;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct PeerScoreContainer {
    by_channel: HashMap<ChannelId, PeerScore>,
}

impl PeerScoreContainer {
    pub fn len(&self) -> usize {
        self.by_channel.len()
    }

    #[cfg(test)]
    pub fn get(&self, channel_id: ChannelId) -> Option<&PeerScore> {
        self.by_channel.get(&channel_id)
    }

    #[cfg(test)]
    pub fn insert(&mut self, channel_id: ChannelId, score: PeerScore) -> Option<PeerScore> {
        self.by_channel.insert(channel_id, score)
    }

    pub fn running_queries(&self, channel_id: ChannelId) -> usize {
        self.by_channel
            .get(&channel_id)
            .map(|p| p.running_queries)
            .unwrap_or_default()
    }

    pub fn request_sent(&mut self, channel_id: ChannelId) {
        self.by_channel
            .entry(channel_id)
            .or_default()
            .request_sent();
    }

    pub fn got_response(&mut self, channel_id: ChannelId) -> bool {
        if let Some(scoring) = self.by_channel.get_mut(&channel_id) {
            scoring.got_response();
            true
        } else {
            false
        }
    }

    pub fn decay(&mut self) {
        for peer in self.by_channel.values_mut() {
            peer.decay();
        }
    }

    pub fn remove(&mut self, channel_id: ChannelId) {
        self.by_channel.remove(&channel_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let container = PeerScoreContainer::default();
        assert_eq!(container.len(), 0);
    }

    #[test]
    fn insert() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id, PeerScore::default());
        assert_eq!(container.len(), 1);
        assert!(container.get(channel_id).is_some())
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
}
