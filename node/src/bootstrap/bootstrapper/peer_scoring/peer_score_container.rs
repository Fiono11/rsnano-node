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

    /// Releases an in-flight query slot without counting it as a response.
    pub fn query_completed(&mut self, channel_id: ChannelId) -> bool {
        self.change_score(channel_id, |score| {
            score.remove_query();
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
    fn query_completed_releases_slot_without_counting_response() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id);
        container.request_sent(channel_id);
        container.request_sent(channel_id);

        let modified = container.query_completed(channel_id);

        assert!(modified);
        assert_eq!(container.get(channel_id).unwrap().running_queries, 1);
        assert_eq!(container.get(channel_id).unwrap().responses, 0);
    }

    #[test]
    fn decay_moves_priority_towards_zero_without_touching_running_queries() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id);
        container.request_sent(channel_id);
        container.request_sent(channel_id);
        let before = container.get(channel_id).unwrap().priority;

        container.decay();

        let score = container.get(channel_id).unwrap();
        // decay only nudges the priority back towards zero...
        assert!(score.priority > before && score.priority < 0.0);
        // ...it must not change the number of in-flight queries
        assert_eq!(score.running_queries, 2);
    }

    #[test]
    fn channel_becomes_unusable_after_too_many_unanswered_requests() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id);

        // Each unanswered request lowers the priority. A couple are still fine...
        container.request_sent(channel_id);
        container.request_sent(channel_id);
        assert!(container.usable.contains(channel_id));

        // ...but the next one pushes the priority below the threshold.
        container.request_sent(channel_id);
        assert!(!container.usable.contains(channel_id));
    }

    #[test]
    fn channel_becomes_usable_again_after_a_response() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id);
        container.request_sent(channel_id);
        container.request_sent(channel_id);
        container.request_sent(channel_id);
        assert!(!container.usable.contains(channel_id));

        // A response both frees a slot and raises the priority back over the threshold.
        container.got_response(channel_id);
        assert!(container.usable.contains(channel_id));
    }

    #[test]
    fn channel_becomes_usable_again_as_priority_decays() {
        let mut container = PeerScoreContainer::default();
        let channel_id = ChannelId::from(42);
        container.insert(channel_id);
        container.request_sent(channel_id);
        container.request_sent(channel_id);
        container.request_sent(channel_id);
        assert!(!container.usable.contains(channel_id));

        // The penalty fades over time until the channel is usable again.
        for _ in 0..100 {
            container.decay();
        }
        assert!(container.usable.contains(channel_id));
    }
}
