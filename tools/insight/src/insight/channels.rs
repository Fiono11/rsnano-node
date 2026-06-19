use std::{
    collections::{HashMap, HashSet},
    net::SocketAddrV6,
    sync::{Arc, RwLock},
};

use rsnano_messages::TelemetryData;
use rsnano_network::{Channel, ChannelDirection, ChannelId};
use rsnano_node::representatives::RepRegistrySnapshot;
use rsnano_types::{Amount, PublicKey};

use crate::insight::message_collection::MessageCollection;

pub(crate) struct ChannelModel {
    pub channel_id: ChannelId,
    pub remote_addr: SocketAddrV6,
    pub direction: ChannelDirection,
    pub telemetry: Option<TelemetryData>,
    pub rep_weight: Amount,
    pub rep_state: RepState,
    pub name: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepState {
    PrincipalRep,
    Rep,
    NoRep,
}

pub(crate) struct Channels {
    channel_map: HashMap<ChannelId, ChannelModel>,
    sorted_channels: Vec<(ChannelId, SocketAddrV6)>,
    selected: Option<ChannelId>,
    selected_index: Option<usize>,
    messages: Arc<RwLock<MessageCollection>>,
    rep_names: HashMap<PublicKey, &'static str>,
}

impl Channels {
    pub(crate) fn new(
        messages: Arc<RwLock<MessageCollection>>,
        rep_names: HashMap<PublicKey, &'static str>,
    ) -> Self {
        Self {
            sorted_channels: Vec::new(),
            channel_map: HashMap::new(),
            selected: None,
            selected_index: None,
            messages,
            rep_names,
        }
    }

    pub(crate) fn update(
        &mut self,
        channels: Vec<Arc<Channel>>,
        telemetries: HashMap<SocketAddrV6, TelemetryData>,
        reps: &RepRegistrySnapshot,
        min_rep_weight: Amount,
    ) {
        let mut inserted = false;
        {
            let mut pending: HashSet<ChannelId> = self.channel_map.keys().cloned().collect();

            for info in channels {
                if let Some(channel) = self.channel_map.get_mut(&info.channel_id()) {
                    channel.telemetry = telemetries.get(&channel.remote_addr).cloned();
                    pending.remove(&info.channel_id());
                } else {
                    self.channel_map.insert(
                        info.channel_id(),
                        ChannelModel {
                            channel_id: info.channel_id(),
                            remote_addr: info.peer_addr(),
                            direction: info.direction(),
                            telemetry: None,
                            rep_weight: Amount::ZERO,
                            rep_state: RepState::NoRep,
                            name: "",
                        },
                    );
                    inserted = true;
                }
            }

            for key in pending {
                self.channel_map.remove(&key);
            }
        }

        if inserted {
            self.sorted_channels = self
                .channel_map
                .values()
                .map(|c| (c.channel_id, c.remote_addr))
                .collect();
            self.sorted_channels.sort_by_key(|c| c.1);
        }

        for channel in self.channel_map.values_mut() {
            channel.rep_weight = Amount::ZERO;
            channel.rep_state = RepState::NoRep;
        }

        for rep in reps.iter() {
            if let Some(channel_id) = rep.channel_id
                && let Some(channel) = self.channel_map.get_mut(&channel_id)
            {
                channel.rep_weight += rep.weight;
                channel.rep_state = if channel.rep_weight > min_rep_weight {
                    RepState::PrincipalRep
                } else if channel.rep_weight > Amount::ZERO {
                    RepState::Rep
                } else {
                    RepState::NoRep
                };
                channel.name = *self.rep_names.get(&rep.public_key).unwrap_or(&"");
            }
        }

        // Recalculate selected index
        if let Some(channel_id) = self.selected {
            match self
                .sorted_channels
                .iter()
                .enumerate()
                .find(|(_, (id, _))| *id == channel_id)
            {
                Some((i, _)) => self.selected_index = Some(i),
                None => {
                    self.selected = None;
                    self.selected_index = None;
                }
            }
        }
    }

    pub(crate) fn get(&self, index: usize) -> Option<&ChannelModel> {
        let (channel_id, _) = self.sorted_channels.get(index)?;
        self.channel_map.get(channel_id)
    }

    pub fn len(&self) -> usize {
        self.sorted_channels.len()
    }

    pub(crate) fn select_index(&mut self, index: usize) {
        let Some((channel_id, _)) = self.sorted_channels.get(index) else {
            return;
        };
        if self.selected == Some(*channel_id) {
            self.selected = None;
            self.selected_index = None;
            self.messages.write().unwrap().filter_channel(None)
        } else {
            self.selected = Some(*channel_id);
            self.selected_index = Some(index);
            self.messages
                .write()
                .unwrap()
                .filter_channel(Some(*channel_id))
        }
    }

    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insight::message_collection::{MessageCollection, MessageFilter};
    use rsnano_node::representatives::{
        RegisteredRep, RegisteredRepSnapshot, RepRegistrySnapshotStub,
    };
    use rsnano_types::PublicKey;
    use std::{collections::HashMap, sync::RwLock};

    #[test]
    fn when_channel_selected_should_set_message_filter() {
        let messages = Arc::new(RwLock::new(MessageCollection::default()));
        let rep_names = HashMap::new();
        let mut channels = Channels::new(messages.clone(), rep_names);
        let reps = RepRegistrySnapshotStub::new();
        channels.update(
            vec![Arc::new(Channel::new_test_instance())],
            HashMap::new(),
            &reps.get(),
            Amount::ZERO,
        );
        channels.select_index(0);
        let guard = messages.read().unwrap();
        assert_eq!(
            guard.current_filter(),
            &MessageFilter::channel(channels.sorted_channels[0].0)
        );
    }

    #[test]
    fn when_channel_deselected_should_clear_message_filter() {
        let messages = Arc::new(RwLock::new(MessageCollection::default()));
        let rep_names = HashMap::new();
        let mut channels = Channels::new(messages.clone(), rep_names);
        let reps = RepRegistrySnapshotStub::new();
        channels.update(
            vec![Arc::new(Channel::new_test_instance())],
            HashMap::new(),
            &reps.get(),
            Amount::ZERO,
        );
        channels.select_index(0);
        channels.select_index(0);
        let guard = messages.read().unwrap();
        assert_eq!(guard.current_filter(), &MessageFilter::all());
    }

    #[test]
    fn shows_representative_name() {
        let messages = Arc::new(RwLock::new(MessageCollection::default()));
        let rep_key = PublicKey::from(42);
        let rep_names: HashMap<PublicKey, &'static str> = [(rep_key.clone(), "test rep")].into();
        let mut channels = Channels::new(messages.clone(), rep_names);
        let channel = Arc::new(Channel::new_test_instance());
        let reps = RepRegistrySnapshotStub::from([RegisteredRepSnapshot {
            rep: RegisteredRep {
                public_key: rep_key,
                channel_id: Some(channel.channel_id()),
                ..RegisteredRep::new_test_instance()
            },
            weight: Amount::nano(1_000_000),
        }]);

        channels.update(vec![channel], HashMap::new(), &reps.get(), Amount::ZERO);

        let channel_model = channels.get(0).unwrap();
        assert_eq!(channel_model.name, "test rep");
    }
}
