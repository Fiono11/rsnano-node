use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use strum::IntoEnumIterator;

use rsnano_messages::{Message, MessageType};
use rsnano_network::{Channel, ChannelEvent, ChannelId};
use rsnano_utils::{
    EventHandler,
    container_info::{ContainerInfo, ContainerInfoProvider},
    fair_queue::FairQueue,
    stats::{StatsCollection, StatsSource},
};

use crate::MessageCallback;

pub struct InboundMessageQueue {
    state: Mutex<State>,
    condition: Condvar,
    inbound_callback: Option<MessageCallback>,
    inbound_dropped_callback: Option<MessageCallback>,
    stats: MsgQueueStats,
}

/// RAI: the lane for the votes of the epoch close elections. They are few
/// (one statement per member per round) and each is needed: a replica that
/// drops one at a full inbound queue is left without the round's
/// certificate, which only solicitations recover, and those replies arrive
/// through the same queue. The lane is drained before the fair queue and
/// is not subject to the per-channel cap, up to its own bound.
const PRIORITY_LANE_MAX: usize = 4096;

fn is_priority(message: &Message) -> bool {
    match message {
        Message::ConfirmAck(ack) => ack.vote().epoch.is_close_round(),
        _ => false,
    }
}

impl InboundMessageQueue {
    pub fn new(max_queue: usize) -> Self {
        Self {
            state: Mutex::new(State {
                queue: FairQueue::new(move |_| max_queue, |_| 1),
                priority: VecDeque::new(),
                stopped: false,
            }),
            condition: Condvar::new(),
            inbound_callback: None,
            inbound_dropped_callback: None,
            stats: Default::default(),
        }
    }

    pub fn set_inbound_callback(&mut self, callback: MessageCallback) {
        self.inbound_callback = Some(callback);
    }

    pub fn set_inbound_dropped_callback(&mut self, callback: MessageCallback) {
        self.inbound_dropped_callback = Some(callback);
    }

    pub fn put(&self, message: Message, channel: Arc<Channel>) -> bool {
        let message_type = message.message_type();
        let added = {
            let mut state = self.state.lock().unwrap();
            if is_priority(&message) && state.priority.len() < PRIORITY_LANE_MAX {
                state
                    .priority
                    .push_back((channel.channel_id(), (message.clone(), channel.clone())));
                true
            } else {
                state
                    .queue
                    .push(channel.channel_id(), (message.clone(), channel.clone()))
            }
        };

        if added {
            self.stats.processed.fetch_add(1, Ordering::Relaxed);
            self.stats.processed_type[message_type as usize].fetch_add(1, Ordering::Relaxed);
            self.condition.notify_one();
            if let Some(cb) = &self.inbound_callback {
                cb(channel.channel_id(), &message);
            }
        } else {
            self.stats.overfill.fetch_add(1, Ordering::Relaxed);
            self.stats.overfill_type[message_type as usize].fetch_add(1, Ordering::Relaxed);
            if let Some(cb) = &self.inbound_dropped_callback {
                cb(channel.channel_id(), &message);
            }
        }

        added
    }

    pub fn next_batch(
        &self,
        max_batch_size: usize,
    ) -> VecDeque<(ChannelId, (Message, Arc<Channel>))> {
        let mut state = self.state.lock().unwrap();
        let first = max_batch_size.min(state.priority.len());
        let mut batch: VecDeque<_> = state.priority.drain(..first).collect();
        if batch.len() < max_batch_size {
            batch.extend(state.queue.next_batch(max_batch_size - batch.len()));
        }
        batch
    }

    pub fn wait_for_messages(&self) {
        let state = self.state.lock().unwrap();
        if !state.is_empty() {
            return;
        }
        drop(
            self.condition
                .wait_while(state, |s| !s.stopped && s.is_empty()),
        )
    }

    pub fn size(&self) -> usize {
        let state = self.state.lock().unwrap();
        state.queue.len() + state.priority.len()
    }

    /// Stop container and notify waiting threads
    pub fn stop(&self) {
        {
            let mut lock = self.state.lock().unwrap();
            lock.stopped = true;
        }
        self.condition.notify_all();
    }
}

impl Default for InboundMessageQueue {
    fn default() -> Self {
        Self::new(64)
    }
}

impl ContainerInfoProvider for InboundMessageQueue {
    fn container_info(&self) -> ContainerInfo {
        let guard = self.state.lock().unwrap();
        ContainerInfo::builder()
            .node("queue", guard.queue.container_info())
            .finish()
    }
}

impl EventHandler<ChannelEvent> for InboundMessageQueue {
    fn handle(&self, event: &ChannelEvent) {
        if let ChannelEvent::Removed(id) = event {
            let mut guard = self.state.lock().unwrap();
            guard.queue.remove(id);
        }
    }
}

struct State {
    queue: FairQueue<ChannelId, (Message, Arc<Channel>)>,
    /// The close-election votes, ahead of everything else
    priority: VecDeque<(ChannelId, (Message, Arc<Channel>))>,
    stopped: bool,
}

impl State {
    fn is_empty(&self) -> bool {
        self.queue.is_empty() && self.priority.is_empty()
    }
}

impl StatsSource for InboundMessageQueue {
    fn collect_stats(&self, result: &mut StatsCollection) {
        self.stats.collect_stats(result);
    }
}

#[derive(Default)]
struct MsgQueueStats {
    processed: AtomicUsize,
    processed_type: [AtomicUsize; MessageType::max_id() + 1],
    overfill: AtomicUsize,
    overfill_type: [AtomicUsize; MessageType::max_id() + 1],
}

impl StatsSource for MsgQueueStats {
    fn collect_stats(&self, result: &mut StatsCollection) {
        result.insert(
            "message_processor",
            "process",
            self.processed.load(Ordering::Relaxed),
        );
        for i in MessageType::iter() {
            result.insert(
                "message_processor_type",
                i.as_str(),
                self.processed_type[i as usize].load(Ordering::Relaxed),
            );
        }
        result.insert(
            "message_processor",
            "overfill",
            self.overfill.load(Ordering::Relaxed),
        );
        for i in MessageType::iter() {
            result.insert(
                "message_processor_overfill",
                i.as_str(),
                self.overfill_type[i as usize].load(Ordering::Relaxed),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_messages::Message;

    /// RAI: a close-election vote is taken in past a full channel queue and
    /// comes out first; an account vote is dropped like any other message
    #[test]
    fn close_election_votes_bypass_a_full_queue() {
        use rsnano_messages::ConfirmAck;
        use rsnano_types::{BlockHash, ConsensusEpoch, PrivateKey, Vote, VoteKind};

        let queue = InboundMessageQueue::new(1);
        let channel = Arc::new(Channel::new_test_instance());
        let vote_in = |epoch| {
            Message::ConfirmAck(ConfirmAck::new_with_own_vote(Vote::new_in_epoch(
                &PrivateKey::from(1),
                VoteKind::First,
                epoch,
                vec![BlockHash::from(7)],
            )))
        };
        assert!(queue.put(Message::BulkPush, channel.clone()));
        assert!(!queue.put(vote_in(ConsensusEpoch::ZERO), channel.clone()));
        assert!(queue.put(
            vote_in(ConsensusEpoch::close_round(ConsensusEpoch::new(1), 0)),
            channel
        ));
        assert_eq!(queue.size(), 2);

        let batch = queue.next_batch(10);
        assert_eq!(batch.len(), 2);
        assert!(matches!(batch[0].1.0, Message::ConfirmAck(_)));
        assert_eq!(queue.size(), 0);
    }

    #[test]
    fn put_and_get_one_message() {
        let manager = InboundMessageQueue::new(1);
        assert_eq!(manager.size(), 0);
        manager.put(Message::BulkPush, Arc::new(Channel::new_test_instance()));
        assert_eq!(manager.size(), 1);
        assert_eq!(manager.next_batch(1000).len(), 1);
        assert_eq!(manager.size(), 0);
    }
}
