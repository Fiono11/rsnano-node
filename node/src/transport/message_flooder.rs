use std::{
    collections::{HashMap, HashSet},
    ops::{Deref, DerefMut},
    sync::{Arc, RwLock},
};

use rsnano_messages::{Message, MessageSerializer};
use rsnano_network::{
    Channel, ChannelDirection, ChannelId, ChannelMode, Network, TEST_ENDPOINT_1, TEST_ENDPOINT_2,
    TrafficType,
};
use rsnano_nullable_clock::Timestamp;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
#[cfg(feature = "rai_protocol")]
use rsnano_types::{NodeId, PublicKey};
use rsnano_utils::stats::Stats;

use super::{MessageSender, try_send_serialized_message};
use crate::representatives::RepresentativeTracker;

/// Floods messages to PRs and non PRs
pub struct MessageFlooder {
    rep_tracker: Arc<RepresentativeTracker>,
    network: Arc<RwLock<Network>>,
    stats: Arc<Stats>,
    message_serializer: MessageSerializer,
    sender: MessageSender,
    flood_listener: OutputListenerMt<FloodEvent>,
    #[cfg(feature = "rai_protocol")]
    committee_node_ids: HashSet<NodeId>,
    #[cfg(feature = "rai_protocol")]
    committee_nodes: HashMap<PublicKey, NodeId>,
}

impl MessageFlooder {
    #[cfg(feature = "rai_protocol")]
    pub fn try_send_to_all_prs_once(&mut self, message: &Message) -> FloodCount {
        let mut result = FloodCount::default();
        let buffer = self.message_serializer.serialize(message);
        let channels: Vec<_> = self
            .network
            .read()
            .unwrap()
            .channels()
            .filter(|channel| {
                self.committee_node_ids.is_empty()
                    || channel
                        .node_id()
                        .is_some_and(|id| self.committee_node_ids.contains(&id))
            })
            .cloned()
            .collect();
        for channel in channels {
            if try_send_serialized_message(
                &channel,
                &self.stats,
                buffer,
                message,
                TrafficType::EpochControl,
            ) {
                result.principal_reps += 1;
            }
        }
        result
    }

    /// Sends a recovery request directly to the node which owns `representative`.
    /// The two committee environment lists are positional pairs, established by
    /// the epoch launcher/configuration.
    #[cfg(feature = "rai_protocol")]
    pub fn try_send_to_rep_once(
        &mut self,
        representative: &PublicKey,
        message: &Message,
    ) -> bool {
        let Some(node_id) = self.committee_nodes.get(representative) else {
            return false;
        };
        let network = self.network.read().unwrap();
        let channel = if network.loopback().node_id() == Some(*node_id) {
            Some(network.loopback().clone())
        } else {
            network.find_node_id(node_id).cloned()
        };
        drop(network);
        let Some(channel) = channel else {
            return false;
        };
        let buffer = self.message_serializer.serialize(message);
        try_send_serialized_message(
            &channel,
            &self.stats,
            buffer,
            message,
            TrafficType::EpochControl,
        )
    }

    #[cfg(feature = "rai_protocol")]
    pub fn try_send_to_random_pr_once(&mut self, message: &Message) -> bool {
        let channel = self
            .network
            .read()
            .unwrap()
            .shuffled_channels(TrafficType::EpochControl)
            .into_iter()
            .find(|channel| {
                channel.is_alive()
                    && channel.mode() == ChannelMode::Established
                    && (self.committee_node_ids.is_empty()
                        || channel
                            .node_id()
                            .is_some_and(|id| self.committee_node_ids.contains(&id)))
            });
        channel.is_some_and(|channel| {
            let buffer = self.message_serializer.serialize(message);
            try_send_serialized_message(
                &channel,
                &self.stats,
                buffer,
                message,
                TrafficType::EpochControl,
            )
        })
    }

    #[cfg(feature = "rai_protocol")]
    pub fn send_to_all_prs_once(&mut self, message: &Message) -> FloodCount {
        let mut result = FloodCount::default();
        // The representative tracker is deliberately sampled and may not contain every PR even
        // in a fully connected committee. Epoch setup establishes a direct channel between every
        // pair. A real peer advertises its listening address in keepalives; injection/RPC test
        // sockets do not. Excluding those ingress-only sockets is important because reliable
        // backpressure on an unread injection socket could otherwise stall delivery to a PR that
        // appears later in this iteration.
        let channels: Vec<_> = self
            .network
            .read()
            .unwrap()
            .channels()
            .filter(|channel| {
                channel.is_alive()
                    && channel.mode() == ChannelMode::Established
                    && (self.committee_node_ids.is_empty()
                        || channel
                            .node_id()
                            .is_some_and(|id| self.committee_node_ids.contains(&id)))
            })
            .cloned()
            .collect();
        for channel in channels {
            if self
                .sender
                .try_send(&channel, message, TrafficType::EpochControl)
            {
                result.principal_reps += 1;
            }
        }
        result
    }
    pub fn new(
        rep_tracker: Arc<RepresentativeTracker>,
        network: Arc<RwLock<Network>>,
        stats: Arc<Stats>,
        sender: MessageSender,
    ) -> Self {
        #[cfg(feature = "rai_protocol")]
        let committee_node_list: Vec<_> = std::env::var("NANO_RAI_NODE_COMMITTEE")
            .unwrap_or_default()
            .split(',')
            .filter_map(|key| PublicKey::decode_hex(key.trim()))
            .map(NodeId::from)
            .collect();
        #[cfg(feature = "rai_protocol")]
        let committee_node_ids = committee_node_list.iter().copied().collect();
        #[cfg(feature = "rai_protocol")]
        let committee_nodes = std::env::var("NANO_RAI_EPOCH_COMMITTEE")
            .unwrap_or_default()
            .split(',')
            .filter_map(|key| PublicKey::decode_hex(key.trim()))
            .zip(committee_node_list)
            .collect();
        Self {
            rep_tracker,
            network,
            stats,
            message_serializer: sender.get_serializer(),
            sender,
            flood_listener: OutputListenerMt::new(),
            #[cfg(feature = "rai_protocol")]
            committee_node_ids,
            #[cfg(feature = "rai_protocol")]
            committee_nodes,
        }
    }

    pub(crate) fn new_null() -> Self {
        let mut network = Network::new_null();
        // add a channel so that capacity checks succeed
        let (channel, _) = network
            .add(
                TEST_ENDPOINT_1,
                TEST_ENDPOINT_2,
                ChannelDirection::Outbound,
                Timestamp::new_test_instance(),
            )
            .unwrap();
        channel.set_mode(rsnano_network::ChannelMode::Established);

        Self::new(
            Arc::new(RepresentativeTracker::default()),
            Arc::new(RwLock::new(network)),
            Arc::new(Stats::default()),
            MessageSender::new_null(),
        )
    }

    pub(crate) fn flood_prs_and_some_non_prs(
        &mut self,
        message: &Message,
        traffic_type: TrafficType,
        scale: f32,
    ) -> FloodCount {
        if self.flood_listener.is_tracked() {
            self.flood_listener.emit(FloodEvent {
                message: message.clone(),
                traffic_type,
                scale,
                all_prs: true,
            });
        }

        let mut flood_count = FloodCount::default();
        let peered_prs = self.rep_tracker.peered_principal_reps();
        for rep in peered_prs {
            if self.try_send_channel_id(rep.channel_id, message, traffic_type) {
                flood_count.principal_reps += 1;
            }
        }

        let mut channels;
        let fanout;
        {
            let network = self.network.read().unwrap();
            fanout = network.fanout(scale);
            channels = network.shuffled_channels(traffic_type)
        }

        self.remove_principal_reps(&mut channels, fanout);
        for peer in channels {
            if self.sender.try_send(&peer, message, traffic_type) {
                flood_count.non_principal_reps += 1;
            }
        }

        flood_count
    }

    pub fn channel(&self, channel_id: ChannelId) -> Option<Arc<Channel>> {
        self.network.read().unwrap().get(channel_id).cloned()
    }

    fn remove_principal_reps(&self, channels: &mut Vec<Arc<Channel>>, count: usize) {
        self.rep_tracker.with_snapshot(|snapshot| {
            channels.retain(|c| !snapshot.is_principal_rep(c.channel_id()));
        });

        channels.truncate(count);
    }

    pub fn try_send_channel_id(
        &mut self,
        channel_id: ChannelId,
        message: &Message,
        traffic_type: TrafficType,
    ) -> bool {
        let Some(channel) = self.network.read().unwrap().get(channel_id).cloned() else {
            return false;
        };
        self.sender.try_send(&channel, message, traffic_type)
    }

    pub fn flood(&mut self, message: &Message, traffic_type: TrafficType, scale: f32) -> usize {
        if self.flood_listener.is_tracked() {
            self.flood_listener.emit(FloodEvent {
                message: message.clone(),
                traffic_type,
                scale,
                all_prs: false,
            });
        }

        let buffer = self.message_serializer.serialize(message);
        let network = self.network.read().unwrap();
        let channels = Self::random_fanout(&network, traffic_type, scale);
        let mut sent = 0;

        for channel in channels {
            if try_send_serialized_message(&channel, &self.stats, buffer, message, traffic_type) {
                sent += 1;
            }
        }
        sent
    }

    pub fn track_floods(&self) -> Arc<OutputTrackerMt<FloodEvent>> {
        self.flood_listener.track()
    }

    fn random_fanout(
        network: &Network,
        traffic_type: TrafficType,
        scale: f32,
    ) -> Vec<Arc<Channel>> {
        let mut channels = network.shuffled_channels(traffic_type);
        channels.truncate(network.fanout(scale));
        channels
    }

    pub fn check_capacity(&self, traffic_type: TrafficType, scale: f32) -> bool {
        self.network
            .read()
            .unwrap()
            .check_capacity(traffic_type, scale)
    }
}

impl Clone for MessageFlooder {
    fn clone(&self) -> Self {
        Self {
            rep_tracker: self.rep_tracker.clone(),
            network: self.network.clone(),
            stats: self.stats.clone(),
            message_serializer: self.message_serializer.clone(),
            sender: self.sender.clone(),
            flood_listener: OutputListenerMt::new(),
            #[cfg(feature = "rai_protocol")]
            committee_node_ids: self.committee_node_ids.clone(),
            #[cfg(feature = "rai_protocol")]
            committee_nodes: self.committee_nodes.clone(),
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, PartialEq, Debug)]
pub struct FloodEvent {
    pub message: Message,
    pub traffic_type: TrafficType,
    pub scale: f32,
    pub all_prs: bool,
}

impl Deref for MessageFlooder {
    type Target = MessageSender;

    fn deref(&self) -> &Self::Target {
        &self.sender
    }
}

impl DerefMut for MessageFlooder {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sender
    }
}

#[derive(Default)]
pub struct FloodCount {
    pub principal_reps: usize,
    pub non_principal_reps: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_track_floods() {
        let mut flooder = MessageFlooder::new_null();
        let tracker = flooder.track_floods();
        let message = Message::BulkPush;
        let traffic_type = TrafficType::Vote;
        let scale = 0.5;
        flooder.flood(&message, traffic_type, scale);

        let floods = tracker.output();
        assert_eq!(
            floods,
            vec![FloodEvent {
                message,
                traffic_type,
                scale,
                all_prs: false
            }]
        );
    }

    #[test]
    fn can_track_floods_to_all_prs() {
        let mut flooder = MessageFlooder::new_null();
        let tracker = flooder.track_floods();
        let message = Message::BulkPush;
        let traffic_type = TrafficType::Vote;
        let scale = 0.5;
        flooder.flood_prs_and_some_non_prs(&message, traffic_type, scale);

        let floods = tracker.output();
        assert_eq!(
            floods,
            vec![FloodEvent {
                message,
                traffic_type,
                scale,
                all_prs: true,
            }]
        );
    }
}
