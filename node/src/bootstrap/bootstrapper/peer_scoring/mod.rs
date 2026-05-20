mod peer_score;
mod peer_score_container;

use std::sync::Mutex;

use rand::seq::SliceRandom;

use rsnano_network::ChannelId;
use rsnano_utils::container_info::ContainerInfo;

use peer_score_container::PeerScoreContainer;

/// Container for tracking and scoring peers with respect to bootstrapping
pub(crate) struct PeerScoring {
    scoring: Mutex<PeerScoreContainer>,
    channel_limit: usize,
}

impl PeerScoring {
    pub fn new(channel_limit: usize) -> Self {
        Self {
            scoring: Mutex::new(PeerScoreContainer::new(channel_limit)),
            channel_limit,
        }
    }

    pub fn channel(&self, mut candidates: Vec<ChannelId>) -> Option<ChannelId> {
        candidates.shuffle(&mut rand::rng());
        let scoring = self.scoring.lock().unwrap();
        candidates
            .iter()
            .find(|channel_id| scoring.running_queries(**channel_id) < self.channel_limit)
            .cloned()
    }

    pub fn request_sent(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().request_sent(channel_id);
    }

    pub fn response_received(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().got_response(channel_id);
    }

    pub fn len(&self) -> usize {
        self.scoring.lock().unwrap().len()
    }

    pub fn decay(&self) {
        self.scoring.lock().unwrap().decay();
    }

    pub fn clean_up_dead_channels(&self, dead_channel_ids: &[ChannelId]) {
        let mut scoring = self.scoring.lock().unwrap();
        for channel_id in dead_channel_ids {
            scoring.remove(*channel_id);
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        [("scores", self.len(), 0)].into()
    }
}

impl Default for PeerScoring {
    fn default() -> Self {
        Self::new(PeerScoreContainer::DEFAULT_CHANNEL_LIMIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn received_message_decrements_running_queries_to_zero() {
        let channel_id = ChannelId::from(1);
        let scoring = PeerScoring::new(16);
        scoring.request_sent(channel_id);

        // Send one query — running_queries becomes 1
        scoring.channel(vec![channel_id]);

        // Receive the response — running_queries should drop to 0
        scoring.response_received(channel_id);

        let inner = scoring.scoring.lock().unwrap();
        let score = inner.get(channel_id).unwrap();
        assert_eq!(score.running_queries, 0);
    }
}
