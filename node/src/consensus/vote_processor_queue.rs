use std::{
    collections::{HashSet, VecDeque},
    mem::size_of,
    sync::{Arc, Condvar, Mutex},
};

use strum::IntoEnumIterator;

use rsnano_network::{Channel, ChannelEvent, ChannelId};
use rsnano_types::{BlockHash, Signature, Vote, VoteDelivery};
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
                local: VecDeque::new(),
                local_seen: HashSet::new(),
                local_history: VecDeque::new(),
                rep_tiers: Default::default(),
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
        let data = self.data.lock().unwrap();
        data.queue.len() + data.local.len()
    }

    pub fn is_empty(&self) -> bool {
        let data = self.data.lock().unwrap();
        data.queue.is_empty() && data.local.is_empty()
    }

    /// Locally signed votes have a separate bounded queue. Backpressure the
    /// generator rather than dropping a statement already broadcast to peers.
    pub(crate) fn enqueue_local(&self, vote: Arc<Vote>) -> bool {
        let mut data = self.data.lock().unwrap();
        while !data.stopped
            && !data.local_seen.contains(&vote.signature)
            && data.local.len() >= self.config.max_pr_queue.max(1)
        {
            data = self.condition.wait(data).unwrap();
        }
        if data.stopped {
            return false;
        }
        if !data.local_seen.insert(vote.signature.clone()) {
            return true;
        }
        data.local_history.push_back(vote.signature.clone());
        if data.local_history.len() > 16_384.max(self.config.max_pr_queue) {
            let oldest = data.local_history.pop_front().unwrap();
            data.local_seen.remove(&oldest);
        }
        data.local.push_back(vote);
        self.condition.notify_all();
        true
    }

    /// Queue vote for processing. @returns true if the vote was queued
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

        let tier;
        let added = {
            let mut guard = self.data.lock().unwrap();
            tier = guard.rep_tiers.tier(&vote.voter);
            guard
                .queue
                .push((tier, channel_id), (vote, source, channel, filter))
        };

        if added {
            self.stats.inc(StatType::VoteProcessor, DetailType::Process);
            self.stats.inc(StatType::VoteProcessorTier, tier.into());
            self.condition.notify_one();
        } else {
            self.stats
                .inc(StatType::VoteProcessor, DetailType::Overfill);
            self.stats.inc(StatType::VoteProcessorOverfill, tier.into());
        }

        added
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

            if !guard.queue.is_empty() || !guard.local.is_empty() {
                let local_limit = if guard.queue.is_empty() {
                    max_batch_size
                } else {
                    max_batch_size.div_ceil(2)
                };
                let mut batch = VecDeque::new();
                for _ in 0..local_limit {
                    let Some(vote) = guard.local.pop_front() else {
                        break;
                    };
                    let tier = guard.rep_tiers.tier(&vote.voter);
                    batch.push_back((
                        (tier, ChannelId::LOOPBACK),
                        (vote, VoteDelivery::Direct, None, None),
                    ));
                }
                if batch.len() < max_batch_size {
                    batch.extend(guard.queue.next_batch(max_batch_size - batch.len()));
                }
                self.condition.notify_all();
                return batch;
            } else {
                guard = self.condition.wait(guard).unwrap();
            }
        }
    }

    pub fn clear(&self) {
        {
            let mut guard = self.data.lock().unwrap();
            guard.queue.clear();
            guard.local.clear();
            guard.local_seen.clear();
            guard.local_history.clear();
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
                guard.queue.len() + guard.local.len(),
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
        }
    }
}

struct VoteProcessorQueueData {
    stopped: bool,
    local: VecDeque<Arc<Vote>>,
    local_seen: HashSet<Signature>,
    local_history: VecDeque<Signature>,
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
}

#[cfg(test)]
mod local_vote_tests {
    use super::*;
    #[test]
    fn local_vote_survives_full_remote_queue() {
        let queue = VoteProcessorQueue::new_null();
        let remote = Arc::new(Vote::null());
        while queue.enqueue(remote.clone(), None, VoteDelivery::Direct, None) {}
        let local = Arc::new(Vote::null());
        assert!(queue.enqueue_local(local.clone()));
        let batch = queue.wait_for_votes(2);
        assert!(Arc::ptr_eq(&batch[0].1.0, &local));
        assert_eq!(batch.len(), 2);
        queue.stop();
        assert!(!queue.enqueue_local(local));
    }
    #[test]
    fn local_recovery_replay_is_deduplicated_after_processing() {
        let queue = VoteProcessorQueue::new_null();
        let vote = Arc::new(Vote::null());
        assert!(queue.enqueue_local(vote.clone()));
        assert_eq!(queue.wait_for_votes(1).len(), 1);
        assert!(queue.enqueue_local(vote));
        assert!(queue.is_empty());
    }

    #[test]
    fn stop_releases_blocked_local_producer() {
        let mut config = VoteProcessorConfig::new(1);
        config.max_pr_queue = 1;
        let queue = Arc::new(VoteProcessorQueue::new(config, Stats::default().into()));
        assert!(queue.enqueue_local(Arc::new(Vote::null())));
        let producer = queue.clone();
        let thread = std::thread::spawn(move || {
            let mut vote = Vote::null();
            vote.signature = Signature::from_bytes([1; 64]);
            producer.enqueue_local(Arc::new(vote))
        });
        queue.stop();
        assert!(!thread.join().unwrap());
    }
}
