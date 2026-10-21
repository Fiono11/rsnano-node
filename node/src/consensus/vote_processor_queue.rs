use std::{
    collections::VecDeque,
    mem::size_of,
    sync::{Arc, Condvar, Mutex},
};

use strum::IntoEnumIterator;

use rsnano_network::{Channel, ChannelEvent, ChannelId};
use rsnano_types::{BlockHash, Vote, VoteDelivery};
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
                close_lane: VecDeque::new(),
                evidence_lane: VecDeque::new(),
                rep_tiers: Default::default(),
                queue: FairQueue::new(
                    move |(tier, channel)| {
                        let max_size = match tier {
                            RepTier::Tier1 | RepTier::Tier2 | RepTier::Tier3 => conf.max_pr_queue,
                            RepTier::None => conf.max_non_pr_queue,
                        };
                        if *channel == ChannelId::LOOPBACK {
                            // allow more votes for LOOPBACK, which comes from the vote cache
                            // and carries the node's own votes. Kudzu: an own vote is a
                            // one-shot statement, dropping it here loses it from the
                            // node's own instance for good, so the queue is deep.
                            {
                                #[cfg(feature = "rai_protocol")]
                                {
                                    max_size * 256
                                }
                                #[cfg(not(feature = "rai_protocol"))]
                                {
                                    max_size * 10
                                }
                            }
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

    /// RAI: the bound of the lane for close-election votes. They are few -
    /// one statement per member and round - and each is needed: a replica
    /// that drops one at a vote queue full of account votes is left without
    /// the round's certificate until the voter repeats it, while the others
    /// have closed the epoch.
    const CLOSE_LANE_MAX: usize = 4096;
    /// RAI: the bound of the lane for votes handed over as certificate
    /// evidence, the answers to this node's own requests. A node that lags
    /// is flooded with live votes; behind them the evidence it asked for is
    /// dropped, and its reports stay unusable while the closes wait for it.
    const EVIDENCE_LANE_MAX: usize = 8192;

    pub fn len(&self) -> usize {
        let guard = self.data.lock().unwrap();
        guard.queue.len() + guard.close_lane.len() + guard.evidence_lane.len()
    }

    pub fn is_empty(&self) -> bool {
        let guard = self.data.lock().unwrap();
        guard.queue.is_empty() && guard.close_lane.is_empty() && guard.evidence_lane.is_empty()
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
            if vote.epoch.is_close_round() && guard.close_lane.len() < Self::CLOSE_LANE_MAX {
                guard
                    .close_lane
                    .push_back(((tier, channel_id), (vote, source, channel, filter)));
                true
            } else if source == VoteDelivery::Evidence
                && guard.evidence_lane.len() < Self::EVIDENCE_LANE_MAX
            {
                guard
                    .evidence_lane
                    .push_back(((tier, channel_id), (vote, source, channel, filter)));
                true
            } else {
                guard
                    .queue
                    .push((tier, channel_id), (vote, source, channel, filter))
            }
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

            if !guard.close_lane.is_empty()
                || !guard.evidence_lane.is_empty()
                || !guard.queue.is_empty()
            {
                let first = max_batch_size.min(guard.close_lane.len());
                let mut batch: VecDeque<QueuedVote> = guard.close_lane.drain(..first).collect();
                let evidence = (max_batch_size - batch.len()).min(guard.evidence_lane.len());
                batch.extend(guard.evidence_lane.drain(..evidence));
                if batch.len() < max_batch_size {
                    let rest = max_batch_size - batch.len();
                    batch.extend(guard.queue.next_batch(rest));
                }
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
            guard.close_lane.clear();
            guard.evidence_lane.clear();
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
        }
    }
}

/// A vote waiting in the queue, with where it came from
type QueuedVote = (
    (RepTier, ChannelId),
    (
        Arc<Vote>,
        VoteDelivery,
        Option<Arc<Channel>>,
        Option<BlockHash>,
    ),
);

struct VoteProcessorQueueData {
    stopped: bool,
    /// RAI: the votes of the epoch close elections, taken past a full fair
    /// queue and drained before it (see `CLOSE_LANE_MAX`)
    close_lane: VecDeque<QueuedVote>,
    /// RAI: the votes handed over as certificate evidence, drained after the
    /// close lane (see `EVIDENCE_LANE_MAX`)
    evidence_lane: VecDeque<QueuedVote>,
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
mod tests {
    use super::*;
    use rsnano_types::{ConsensusEpoch, PrivateKey, VoteKind};

    /// RAI: a close-election vote is taken in past a full vote queue and
    /// comes out first; an account vote is dropped like any other
    #[test]
    fn close_votes_bypass_a_full_queue() {
        let mut config = VoteProcessorConfig::new(1);
        config.max_pr_queue = 1;
        config.max_non_pr_queue = 1;
        let queue = VoteProcessorQueue::new(config, Arc::new(Stats::default()));
        let channel = Some(Arc::new(Channel::new_test_instance()));
        let vote_in = |epoch| {
            Arc::new(Vote::new_in_epoch(
                &PrivateKey::from(1),
                VoteKind::First,
                epoch,
                vec![BlockHash::from(7)],
            ))
        };
        let account = || vote_in(ConsensusEpoch::ZERO);
        let close = vote_in(ConsensusEpoch::close_round(ConsensusEpoch::new(1), 0));
        assert!(queue.enqueue(account(), channel.clone(), VoteDelivery::Direct, None));
        assert!(!queue.enqueue(account(), channel.clone(), VoteDelivery::Direct, None));
        assert!(queue.enqueue(close.clone(), channel, VoteDelivery::Direct, None));
        assert_eq!(queue.len(), 2);

        let batch = queue.wait_for_votes(10);
        assert_eq!(batch.len(), 2);
        assert!(Arc::ptr_eq(&(batch[0].1).0, &close));
        assert!(queue.is_empty());
    }

    /// RAI: a vote handed over as certificate evidence is taken in past a
    /// full vote queue, after the close votes
    #[test]
    fn evidence_votes_bypass_a_full_queue() {
        let mut config = VoteProcessorConfig::new(1);
        config.max_pr_queue = 1;
        config.max_non_pr_queue = 1;
        let queue = VoteProcessorQueue::new(config, Arc::new(Stats::default()));
        let channel = Some(Arc::new(Channel::new_test_instance()));
        let vote = || {
            Arc::new(Vote::new_in_epoch(
                &PrivateKey::from(1),
                VoteKind::Final,
                ConsensusEpoch::ZERO,
                vec![BlockHash::from(7)],
            ))
        };
        assert!(queue.enqueue(vote(), channel.clone(), VoteDelivery::Direct, None));
        assert!(!queue.enqueue(vote(), channel.clone(), VoteDelivery::Direct, None));
        let evidence = vote();
        assert!(queue.enqueue(evidence.clone(), channel, VoteDelivery::Evidence, None));
        let batch = queue.wait_for_votes(10);
        assert_eq!(batch.len(), 2);
        assert!(Arc::ptr_eq(&(batch[0].1).0, &evidence));
    }
}
