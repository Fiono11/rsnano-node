use std::{
    ops::Deref,
    sync::{Arc, Mutex},
};
#[cfg(feature = "rai_protocol")]
use std::{thread::sleep, time::Duration};

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
    fn enqueue_local(&self, vote: Arc<Vote>) {
        #[cfg(not(feature = "rai_protocol"))]
        let _ = self
            .vote_processor_queue
            .enqueue(vote, None, VoteDelivery::Direct, None);
        #[cfg(feature = "rai_protocol")]
        while !self.vote_processor_queue.stopped()
            && !self
                .vote_processor_queue
                .enqueue(vote.clone(), None, VoteDelivery::Direct, None)
        {
            sleep(Duration::from_millis(1));
        }
    }

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
        let ack = Message::ConfirmAck(ConfirmAck::new_with_own_vote(vote.deref().clone()));

        let stat_type = if vote.is_final() {
            StatType::VoteGeneratorFinal
        } else {
            StatType::VoteGenerator
        };

        self.enqueue_local(vote);

        let mut flooder = self.message_flooder.lock().unwrap();
        #[cfg(feature = "rai_protocol")]
        let count = flooder.send_to_all_prs_once(&ack);
        #[cfg(not(feature = "rai_protocol"))]
        let count = flooder.flood_prs_and_some_non_prs(&ack, TrafficType::Vote, 2.0);

        self.stats
            .add(stat_type, DetailType::SentPr, count.principal_reps as u64);
        self.stats.add(
            stat_type,
            DetailType::SentNonPr,
            count.non_principal_reps as u64,
        );
    }

    #[cfg(feature = "rai_protocol")]
    pub fn reply_recovery(
        &self,
        vote: Arc<Vote>,
        channel_id: rsnano_network::ChannelId,
        apply_local: bool,
    ) {
        // Recovery is requester-driven. One successful reply must not suppress delivery of the
        // same signed phase to another PR which is still missing it.
        let ack = Message::ConfirmAck(ConfirmAck::new_with_recovery_vote(vote.as_ref().clone()));
        if apply_local {
            self.enqueue_local(vote);
        }
        self.message_flooder.lock().unwrap().try_send_channel_id(
            channel_id,
            &ack,
            TrafficType::VoteReply,
        );
    }
}
