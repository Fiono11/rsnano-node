use std::{
    ops::Deref,
    sync::{Arc, Mutex},
};

use rsnano_messages::{ConfirmAck, Message};
use rsnano_network::TrafficType;
use rsnano_types::{Vote, VoteDelivery};
use rsnano_utils::stats::{DetailType, StatType, Stats};

use super::{VoteProcessorConfig, VoteProcessorQueue};
use crate::transport::MessageFlooder;

/// Broadcast a vote to PRs and some non-PRs
pub struct VoteBroadcaster {
    vote_processor_queue: Arc<VoteProcessorQueue>,
    message_flooder: Mutex<MessageFlooder>,
    stats: Arc<Stats>,
}

impl VoteBroadcaster {
    pub fn new(
        vote_processor_queue: Arc<VoteProcessorQueue>,
        message_flooder: MessageFlooder,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            vote_processor_queue,
            message_flooder: Mutex::new(message_flooder),
            stats,
        }
    }

    pub fn new_null() -> Self {
        let stats = Arc::new(Stats::default());
        let queue = Arc::new(VoteProcessorQueue::new(
            VoteProcessorConfig::new(1),
            stats.clone(),
        ));
        let flooder = MessageFlooder::new_null();
        Self::new(queue, flooder, stats)
    }

    /// Broadcast vote to PRs and some non-PRs
    pub fn broadcast(&self, vote: Arc<Vote>) {
        self.broadcast_with(vote, TrafficType::Vote, 2.0);
    }

    /// Close votes are bounded protocol-control traffic. Keep them off the
    /// saturated ordinary vote queue so all committee members can derive the
    /// split/death certificates required to advance a replica-relative close
    /// round while the workload is still producing slot votes.
    #[cfg(feature = "rai_protocol")]
    pub fn broadcast_rai_close(&self, vote: Arc<Vote>) {
        self.broadcast_with(vote, TrafficType::VoteReply, 8.0);
    }

    fn broadcast_with(&self, vote: Arc<Vote>, traffic_type: TrafficType, scale: f32) {
        let ack = Message::ConfirmAck(ConfirmAck::new_with_own_vote(vote.deref().clone()));

        let stat_type = if vote.is_final() {
            StatType::VoteGeneratorFinal
        } else {
            StatType::VoteGenerator
        };

        self.vote_processor_queue
            .enqueue(vote, None, VoteDelivery::Direct, None);

        let count = self
            .message_flooder
            .lock()
            .unwrap()
            .flood_prs_and_some_non_prs(&ack, traffic_type, scale);

        self.stats
            .add(stat_type, DetailType::SentPr, count.principal_reps as u64);
        self.stats.add(
            stat_type,
            DetailType::SentNonPr,
            count.non_principal_reps as u64,
        );
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use rsnano_types::{BlockHash, PrivateKey, UnixMillisTimestamp};

    use super::*;

    #[test]
    fn close_vote_uses_control_traffic_flooding() {
        let broadcaster = VoteBroadcaster::new_null();
        let tracker = broadcaster.message_flooder.lock().unwrap().track_floods();
        let vote = Arc::new(Vote::new(
            &PrivateKey::from(1),
            UnixMillisTimestamp::new(1),
            0,
            vec![BlockHash::from(2)],
        ));

        broadcaster.broadcast_rai_close(vote);

        let output = tracker.output();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].traffic_type, TrafficType::VoteReply);
        assert_eq!(output[0].scale, 8.0);
        assert!(output[0].all_prs);
    }
}
