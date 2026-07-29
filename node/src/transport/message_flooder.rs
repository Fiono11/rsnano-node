use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, RwLock},
};

use rsnano_messages::{Message, MessageSerializer};
use rsnano_network::{
    Channel, ChannelDirection, ChannelId, Network, TEST_ENDPOINT_1, TEST_ENDPOINT_2, TrafficType,
};
use rsnano_nullable_clock::Timestamp;
use rsnano_output_tracker::{OutputListenerMt, OutputTrackerMt};
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
}

impl MessageFlooder {
    pub fn new(
        rep_tracker: Arc<RepresentativeTracker>,
        network: Arc<RwLock<Network>>,
        stats: Arc<Stats>,
        sender: MessageSender,
    ) -> Self {
        Self {
            rep_tracker,
            network,
            stats,
            message_serializer: sender.get_serializer(),
            sender,
            flood_listener: OutputListenerMt::new(),
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

    /// Sends a durable protocol object to every currently established peer.
    /// This is intended for infrequent evidence whose eventual availability
    /// cannot depend on probabilistic fanout.
    pub fn flood_all(&mut self, message: &Message, traffic_type: TrafficType) -> usize {
        if self.flood_listener.is_tracked() {
            self.flood_listener.emit(FloodEvent {
                message: message.clone(),
                traffic_type,
                scale: 1.0,
                all_prs: false,
            });
        }

        let buffer = self.message_serializer.serialize(message);
        let channels = self.network.read().unwrap().shuffled_channels(traffic_type);
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

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn flood_skips_pre_rai_peers() {
        use rsnano_types::{
            Account, BlockHash, PrivateKey, RaiElectionId, RaiElectionValue, RaiSlot, RaiVote,
        };

        let mut flooder = MessageFlooder::new_null();
        let key = PrivateKey::from(1);
        let message = Message::RaiVote(RaiVote::new_first(
            &key,
            RaiElectionId::Slot {
                slot: RaiSlot::new(Account::from(2), 3),
                epoch: 4,
            },
            RaiElectionValue::Block(BlockHash::from(5)),
        ));

        let sent = flooder.flood(&message, TrafficType::Generic, 1.0);

        assert_eq!(sent, 0);
    }
}
