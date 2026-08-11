use std::{
    collections::VecDeque,
    mem::size_of,
    sync::{Arc, Condvar, Mutex},
};

#[cfg(feature = "rai_protocol")]
use std::collections::HashMap;

use strum::IntoEnumIterator;

use rsnano_network::{Channel, ChannelEvent, ChannelId};
use rsnano_types::{BlockHash, Vote, VoteDelivery};
#[cfg(feature = "rai_protocol")]
use rsnano_types::{PublicKey, Signature};
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    fair_queue::{FairQueue, FairQueueInfo},
    stats::{DetailType, StatType, Stats},
};

use super::{RepTier, RepTiers, RepTiersConsumer, VoteProcessorConfig};

pub struct VoteProcessorQueue {
    data: Mutex<VoteProcessorQueueData>,
    condition: Condvar,
    pub config: VoteProcessorConfig,
    stats: Arc<Stats>,
}

impl VoteProcessorQueue {
    pub fn new(config: VoteProcessorConfig, stats: Arc<Stats>) -> Self {
        let conf = config.clone();
        Self {
            data: Mutex::new(VoteProcessorQueueData {
                stopped: false,
                rep_tiers: Default::default(),
                #[cfg(feature = "rai_protocol")]
                forwarded_votes: Default::default(),
                queue: FairQueue::new(
                    move |(tier, channel)| {
                        let max_size = match tier {
                            RepTier::Tier1 | RepTier::Tier2 | RepTier::Tier3 => conf.max_pr_queue,
                            RepTier::None => conf.max_non_pr_queue,
                        };
                        if *channel == ChannelId::LOOPBACK {
                            // allow more votes for LOOPBACK, which comes from the vote cache!
                            max_size * 10
                        } else {
                            max_size
                        }
                    },
                    move |(tier, _)| match tier {
                        RepTier::Tier3 => conf.pr_priority * conf.pr_priority * conf.pr_priority,
                        RepTier::Tier2 => conf.pr_priority * conf.pr_priority,
                        RepTier::Tier1 => conf.pr_priority,
                        RepTier::None => 1,
                    },
                ),
            }),
            condition: Condvar::new(),
            config,
            stats,
        }
    }

    pub fn new_null() -> Self {
        Self::new(VoteProcessorConfig::new(1), Stats::default().into())
    }

    pub fn len(&self) -> usize {
        self.data.lock().unwrap().queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.lock().unwrap().queue.is_empty()
    }

    /// Queue a vote for processing. Returns true when the vote was queued or an
    /// identical forwarded copy is already queued.
    pub fn enqueue(
        &self,
        vote: Arc<Vote>,
        channel: Option<Arc<Channel>>,
        source: VoteDelivery,
        filter: Option<BlockHash>,
    ) -> bool {
        let channel_id = match &channel {
            Some(channel) => channel.channel_id(),
            None => ChannelId::LOOPBACK,
        };

        let (tier, added, duplicate) = {
            let mut guard = self.data.lock().unwrap();
            let tier = guard.rep_tiers.tier(&vote.voter);

            #[cfg(feature = "rai_protocol")]
            {
                // RAI deliberately permits a signed vote to be retried after it
                // was first seen, because an election may only become
                // actionable later. However, processing several forwarded
                // copies of the same vote while one is already waiting adds no
                // evidence and amplifies scalar-vote gossip. Coalesce only that
                // transient case; direct and vote-cache deliveries retain their
                // channel/replay semantics, and the key is removed after the
                // retained vote has been processed.
                let key = forwarded_vote_key(&vote, filter);
                let duplicate =
                    source == VoteDelivery::Forwarded && guard.forwarded_votes.contains_key(&key);
                if duplicate {
                    (tier, false, true)
                } else {
                    let added = guard
                        .queue
                        .push((tier, channel_id), (vote, source, channel, filter));
                    if added && source == VoteDelivery::Forwarded {
                        guard.forwarded_votes.insert(key, Some(channel_id));
                    }
                    (tier, added, false)
                }
            }

            #[cfg(not(feature = "rai_protocol"))]
            {
                let added = guard
                    .queue
                    .push((tier, channel_id), (vote, source, channel, filter));
                (tier, added, false)
            }
        };

        if added {
            self.stats.inc(StatType::VoteProcessor, DetailType::Process);
            self.stats.inc(StatType::VoteProcessorTier, tier.into());
            self.condition.notify_one();
        } else if duplicate {
            self.stats
                .inc(StatType::VoteProcessor, DetailType::Duplicate);
        } else {
            self.stats
                .inc(StatType::VoteProcessor, DetailType::Overfill);
            self.stats.inc(StatType::VoteProcessorOverfill, tier.into());
        }

        // A coalesced copy is accepted because an identical queued vote will be
        // processed; callers should not treat it as overfill and solicit it
        // again immediately.
        added || duplicate
    }

    pub(crate) fn wait_for_votes(
        &self,
        max_batch_size: usize,
    ) -> VecDeque<(
        (RepTier, ChannelId),
        (
            Arc<Vote>,
            VoteDelivery,
            Option<Arc<Channel>>,
            Option<BlockHash>,
        ),
    )> {
        let mut guard = self.data.lock().unwrap();
        loop {
            if guard.stopped {
                return VecDeque::new();
            }

            if !guard.queue.is_empty() {
                let batch = guard.queue.next_batch(max_batch_size);
                #[cfg(feature = "rai_protocol")]
                for (_, (vote, source, _, filter)) in &batch {
                    if *source == VoteDelivery::Forwarded
                        && let Some(channel) = guard
                            .forwarded_votes
                            .get_mut(&forwarded_vote_key(vote, *filter))
                    {
                        // Keep the key while the vote is in flight, but no
                        // longer associate it with a removable channel queue.
                        *channel = None;
                    }
                }
                return batch;
            } else {
                guard = self.condition.wait(guard).unwrap();
            }
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub(crate) fn forwarded_vote_processed(&self, vote: &Vote, filter: Option<BlockHash>) {
        self.data
            .lock()
            .unwrap()
            .forwarded_votes
            .remove(&forwarded_vote_key(vote, filter));
    }

    pub fn clear(&self) {
        {
            let mut guard = self.data.lock().unwrap();
            guard.queue.clear();
            #[cfg(feature = "rai_protocol")]
            // Preserve in-flight keys so a concurrent clear cannot admit a
            // second copy before the retained vote finishes processing.
            guard
                .forwarded_votes
                .retain(|_, queued_channel| queued_channel.is_none());
        }
        self.condition.notify_all();
    }

    pub fn stop(&self) {
        {
            let mut guard = self.data.lock().unwrap();
            guard.stopped = true;
        }
        self.condition.notify_all();
    }

    pub fn stopped(&self) -> bool {
        self.data.lock().unwrap().stopped
    }

    pub fn info(&self) -> FairQueueInfo<RepTier> {
        self.data
            .lock()
            .unwrap()
            .queue
            .compacted_info(|(tier, _)| *tier)
    }
}

impl ContainerInfoProvider for VoteProcessorQueue {
    fn container_info(&self) -> ContainerInfo {
        let guard = self.data.lock().unwrap();
        ContainerInfo::builder()
            .leaf(
                "votes",
                guard.queue.len(),
                size_of::<(Arc<Vote>, VoteDelivery)>(),
            )
            .node("queue", guard.queue.container_info())
            .finish()
    }
}

impl RepTiersConsumer for VoteProcessorQueue {
    fn update_rep_tiers(&self, new_tiers: RepTiers) {
        self.data.lock().unwrap().rep_tiers = new_tiers;
    }
}

impl EventHandler<ChannelEvent> for VoteProcessorQueue {
    fn handle(&self, event: &ChannelEvent) {
        if let ChannelEvent::Removed(id) = event {
            let mut guard = self.data.lock().unwrap();
            for tier in RepTier::iter() {
                guard.queue.remove(&(tier, *id));
            }
            #[cfg(feature = "rai_protocol")]
            guard
                .forwarded_votes
                .retain(|_, queued_channel| *queued_channel != Some(*id));
        }
    }
}

struct VoteProcessorQueueData {
    stopped: bool,
    queue: FairQueue<
        (RepTier, ChannelId),
        (
            Arc<Vote>,
            VoteDelivery,
            Option<Arc<Channel>>,
            Option<BlockHash>, //filter
        ),
    >,
    rep_tiers: RepTiers,
    #[cfg(feature = "rai_protocol")]
    /// A value of `Some(channel)` identifies a queued copy. `None` means the
    /// retained copy has been dequeued and is currently being processed.
    forwarded_votes: HashMap<ForwardedVoteKey, Option<ChannelId>>,
}

#[cfg(feature = "rai_protocol")]
type ForwardedVoteKey = (PublicKey, Signature, BlockHash, BlockHash);

#[cfg(feature = "rai_protocol")]
fn forwarded_vote_key(vote: &Vote, filter: Option<BlockHash>) -> ForwardedVoteKey {
    (
        vote.voter,
        vote.signature.clone(),
        vote.hash(),
        filter.unwrap_or_default(),
    )
}

#[cfg(all(test, feature = "rai_protocol"))]
mod rai_tests {
    use super::*;

    #[test]
    fn coalesces_queued_and_in_flight_forwarded_votes() {
        let stats = Arc::new(Stats::default());
        let queue = VoteProcessorQueue::new(VoteProcessorConfig::new(1), stats.clone());
        let vote = Arc::new(Vote::new_test_instance());
        let first_channel = Arc::new(Channel::new_test_instance_with_id(1));
        let second_channel = Arc::new(Channel::new_test_instance_with_id(2));

        assert!(queue.enqueue(
            vote.clone(),
            Some(first_channel),
            VoteDelivery::Forwarded,
            None,
        ));
        assert!(queue.enqueue(
            vote.clone(),
            Some(second_channel.clone()),
            VoteDelivery::Forwarded,
            None,
        ));
        assert_eq!(queue.len(), 1);
        assert_eq!(
            stats.count(
                StatType::VoteProcessor,
                DetailType::Duplicate,
                rsnano_utils::stats::Direction::In,
            ),
            1
        );

        assert_eq!(queue.wait_for_votes(1).len(), 1);

        // The retained key covers in-flight processing as well as queueing.
        queue.clear();
        assert!(queue.enqueue(
            vote.clone(),
            Some(second_channel.clone()),
            VoteDelivery::Forwarded,
            None,
        ));
        assert!(queue.is_empty());

        queue.forwarded_vote_processed(&vote, None);

        // Once the retained copy leaves the queue, a later repair retry must
        // be admitted and reconsidered against the then-current election state.
        assert!(queue.enqueue(vote, Some(second_channel), VoteDelivery::Forwarded, None,));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn does_not_coalesce_direct_or_replayed_votes() {
        let queue = VoteProcessorQueue::new_null();
        let vote = Arc::new(Vote::new_test_instance());

        assert!(queue.enqueue(vote.clone(), None, VoteDelivery::Direct, None));
        assert!(queue.enqueue(vote.clone(), None, VoteDelivery::Direct, None));
        assert!(queue.enqueue(
            vote.clone(),
            None,
            VoteDelivery::Replayed,
            Some(BlockHash::from(1)),
        ));
        assert!(queue.enqueue(vote, None, VoteDelivery::Replayed, Some(BlockHash::from(1)),));

        assert_eq!(queue.len(), 4);
    }

    #[test]
    fn does_not_coalesce_different_payloads_with_a_copied_signature() {
        let queue = VoteProcessorQueue::new_null();
        let vote = Arc::new(Vote::new_test_instance());
        let mut altered_vote = vote.as_ref().clone();
        altered_vote.hashes[0] = BlockHash::from(999);

        assert!(queue.enqueue(vote, None, VoteDelivery::Forwarded, None));
        assert!(queue.enqueue(Arc::new(altered_vote), None, VoteDelivery::Forwarded, None,));

        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn channel_removal_does_not_leave_stale_forwarded_vote_keys() {
        let queue = VoteProcessorQueue::new_null();
        let vote = Arc::new(Vote::new_test_instance());
        let removed_channel = Arc::new(Channel::new_test_instance_with_id(1));
        let surviving_channel = Arc::new(Channel::new_test_instance_with_id(2));

        assert!(queue.enqueue(
            vote.clone(),
            Some(removed_channel.clone()),
            VoteDelivery::Forwarded,
            None,
        ));
        queue.handle(&ChannelEvent::Removed(removed_channel.channel_id()));
        assert!(queue.is_empty());

        assert!(queue.enqueue(vote, Some(surviving_channel), VoteDelivery::Forwarded, None,));
        assert_eq!(queue.len(), 1);
    }
}
