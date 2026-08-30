mod peer_score;
mod peer_score_container;

use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use rand::seq::IndexedRandom;

use rsnano_network::{ChannelEvent, ChannelId};
use rsnano_utils::{EventHandler, container_info::ContainerInfo};

use peer_score_container::PeerScoreContainer;

/// Container for tracking and scoring peers with respect to bootstrapping
pub(crate) struct PeerScoring {
    scoring: Mutex<PeerScoreContainer>,
    frontier_cursor: AtomicUsize,
}

impl PeerScoring {
    pub fn new(channel_limit: usize) -> Self {
        Self {
            scoring: Mutex::new(PeerScoreContainer::new(channel_limit)),
            frontier_cursor: AtomicUsize::new(0),
        }
    }

    #[cfg(test)]
    pub fn insert(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().insert(channel_id);
    }

    pub fn channel(&self) -> Option<ChannelId> {
        let scoring = self.scoring.lock().unwrap();
        scoring.usable().choose(&mut rand::rng()).cloned()
    }

    #[cfg(feature = "rai_protocol")]
    pub fn frontier_channel(&self) -> Option<ChannelId> {
        let scoring = self.scoring.lock().unwrap();
        let usable = scoring.usable();
        if usable.is_empty() {
            return None;
        }
        let index = self.frontier_cursor.fetch_add(1, Ordering::Relaxed) % usable.len();
        Some(usable[index])
    }

    pub fn channel_full(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().channel_full(channel_id);
    }

    pub fn request_sent(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().request_sent(channel_id);
    }

    pub fn response_received(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().got_response(channel_id);
    }

    /// Releases a peer's in-flight slot when its running query is consumed
    /// without counting as a valid response (e.g. wrong response type).
    pub fn query_completed(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().query_completed(channel_id);
    }

    pub fn blocks_received(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().blocks_received(channel_id);
    }

    pub fn out_of_date(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().out_of_date(channel_id);
    }

    pub fn len(&self) -> usize {
        self.scoring.lock().unwrap().len()
    }

    pub fn decay(&self) {
        self.scoring.lock().unwrap().decay();
    }

    pub fn timed_out(&self, channel_id: ChannelId) {
        self.scoring.lock().unwrap().timed_out(channel_id);
    }

    pub fn container_info(&self) -> ContainerInfo {
        [("scores", self.len(), 0)].into()
    }

    pub fn snapshot(&self) -> Vec<PeerScoreSnapshot> {
        let mut snapshots: Vec<_> = self
            .scoring
            .lock()
            .unwrap()
            .iter()
            .map(|(channel_id, score)| PeerScoreSnapshot {
                channel_id: *channel_id,
                running_queries: score.running_queries,
                priority: score.priority,
                timeouts: score.timeouts,
                requests: score.requests,
                responses: score.responses,
                channel_full: score.channel_full,
                blocks_received: score.blocks_received,
                out_of_date: score.out_of_date,
            })
            .collect();

        snapshots.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));

        snapshots
    }
}

impl Default for PeerScoring {
    fn default() -> Self {
        Self::new(PeerScoreContainer::DEFAULT_CHANNEL_LIMIT)
    }
}

impl EventHandler<ChannelEvent> for PeerScoring {
    fn handle(&self, event: &ChannelEvent) {
        match event {
            ChannelEvent::Established(channel) => {
                self.scoring.lock().unwrap().insert(channel.channel_id());
            }
            ChannelEvent::Removed(channel_id) => {
                self.scoring.lock().unwrap().remove(*channel_id);
            }
        }
    }
}

#[derive(Clone)]
pub struct PeerScoreSnapshot {
    pub channel_id: ChannelId,
    pub running_queries: usize,
    pub priority: f32,
    pub timeouts: usize,
    pub requests: usize,
    pub responses: usize,
    pub channel_full: usize,
    pub blocks_received: usize,
    pub out_of_date: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn received_message_decrements_running_queries_to_zero() {
        let channel_id = ChannelId::from(1);
        let scoring = PeerScoring::new(16);
        scoring.insert(channel_id);

        // Send one query — running_queries becomes 1
        scoring.request_sent(channel_id);

        // Receive the response — running_queries should drop to 0
        scoring.response_received(channel_id);

        let inner = scoring.scoring.lock().unwrap();
        let score = inner.get(channel_id).unwrap();
        assert_eq!(score.running_queries, 0);
    }
}
