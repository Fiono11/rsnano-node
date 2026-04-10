use rand::seq::SliceRandom;

use rsnano_network::ChannelId;
use rsnano_utils::container_info::ContainerInfo;

use super::{peer_score::PeerScore, peer_score_container::PeerScoreContainer};

/// Container for tracking and scoring peers with respect to bootstrapping
pub(crate) struct PeerScoring {
    scoring: PeerScoreContainer,
    channel_limit: usize,
}

impl PeerScoring {
    pub fn new() -> Self {
        Self {
            scoring: PeerScoreContainer::default(),
            channel_limit: 16,
        }
    }

    pub fn set_channel_limit(&mut self, limit: usize) {
        self.channel_limit = limit;
    }

    pub fn received_message(&mut self, channel_id: ChannelId) {
        self.scoring.modify(channel_id, |i| i.got_response());
    }

    pub fn channel(&mut self, mut candidates: Vec<ChannelId>) -> Option<ChannelId> {
        candidates.shuffle(&mut rand::rng());
        candidates
            .iter()
            .find(|channel_id| self.scoring.running_queries(**channel_id) < self.channel_limit)
            .cloned()
    }

    pub fn add_query(&mut self, channel_id: ChannelId) {
        self.scoring.add_query(channel_id);
    }

    pub fn len(&self) -> usize {
        self.scoring.len()
    }

    pub fn decay(&mut self) {
        self.scoring.modify_all(|i| i.decay());
    }

    pub fn clean_up_dead_channels(&mut self, dead_channel_ids: &[ChannelId]) {
        for channel_id in dead_channel_ids {
            self.scoring.remove(*channel_id);
        }
    }

    pub fn container_info(&self) -> ContainerInfo {
        [("scores", self.len(), 0)].into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn received_message_decrements_running_queries_to_zero() {
        let channel_id = ChannelId::from(1);
        let mut scoring = PeerScoring::new();
        scoring.add_query(channel_id);

        // Send one query — running_queries becomes 1
        scoring.channel(vec![channel_id]);

        // Receive the response — running_queries should drop to 0
        scoring.received_message(channel_id);

        let score = scoring.scoring.get(channel_id).unwrap();
        assert_eq!(score.running_queries, 0);
        assert_eq!(score.response_count_total, 1);
    }
}
