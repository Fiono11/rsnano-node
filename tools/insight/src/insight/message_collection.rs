use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};

use rsnano_messages::{AscPullAckType, AscPullReqType, HashType, Message, MessageType};
use rsnano_network::{ChannelDirection, ChannelId};
use rsnano_types::{Account, BlockHash};

#[derive(Clone)]
pub(crate) struct RecordedMessage {
    pub channel_id: ChannelId,
    pub message: Message,
    pub direction: ChannelDirection,
    pub date: DateTime<Utc>,
}

impl RecordedMessage {
    #[allow(dead_code)]
    pub fn new_test_instance() -> Self {
        Self {
            channel_id: 42.into(),
            message: Message::BulkPush,
            direction: ChannelDirection::Outbound,
            date: Utc.with_ymd_and_hms(2024, 10, 18, 12, 59, 0).unwrap(),
        }
    }
}

#[derive(Default, PartialEq, Eq, Debug, Clone)]
pub(crate) struct MessageFilter {
    channel_id: Option<ChannelId>,
    hash: Option<BlockHash>,
    account: Option<Account>,
    type_direction_pairs: Vec<(MessageType, ChannelDirection)>,
}

impl MessageFilter {
    #[allow(dead_code)]
    pub fn all() -> Self {
        Default::default()
    }

    #[allow(dead_code)]
    pub fn channel(channel_id: ChannelId) -> Self {
        Self {
            channel_id: Some(channel_id),
            ..Default::default()
        }
    }

    pub fn include(&self, message: &RecordedMessage) -> bool {
        self.include_channel(message)
            && self.include_type_direction(message)
            && self.include_message_content(message)
    }

    pub fn with_type_direction_pairs(&self, pairs: Vec<(MessageType, ChannelDirection)>) -> Self {
        Self {
            type_direction_pairs: pairs,
            ..self.clone()
        }
    }

    pub fn with_channel(&self, channel_id: Option<ChannelId>) -> Self {
        Self {
            channel_id,
            ..self.clone()
        }
    }

    pub fn with_hash(&self, hash: Option<BlockHash>) -> Self {
        Self {
            hash,
            ..self.clone()
        }
    }

    pub fn with_account(&self, account: Option<Account>) -> Self {
        Self {
            account,
            ..self.clone()
        }
    }

    fn include_channel(&self, message: &RecordedMessage) -> bool {
        match self.channel_id {
            Some(id) => message.channel_id == id,
            None => true,
        }
    }

    fn include_type_direction(&self, message: &RecordedMessage) -> bool {
        if self.type_direction_pairs.is_empty() {
            return true;
        }
        self.type_direction_pairs
            .iter()
            .any(|(t, d)| message.message.message_type() == *t && message.direction == *d)
    }

    fn include_message_content(&self, message: &RecordedMessage) -> bool {
        self.include_hash(message) && self.include_account(message)
    }

    fn include_hash(&self, message: &RecordedMessage) -> bool {
        let Some(hash) = self.hash else {
            return true;
        };
        match &message.message {
            Message::Publish(i) => i.block.hash() == hash,
            Message::AscPullAck(ack) => match &ack.pull_type {
                AscPullAckType::Blocks(i) => i.blocks().iter().any(|b| b.hash() == hash),
                AscPullAckType::AccountInfo(i) => {
                    i.account_open == hash
                        || i.account_head == hash
                        || i.account_conf_frontier == hash
                }
                AscPullAckType::Frontiers(i) => i.iter().any(|f| f.hash == hash),
            },
            Message::AscPullReq(req) => match &req.req_type {
                AscPullReqType::Blocks(i) => {
                    i.start_type == HashType::Block && hash == i.start.into()
                }
                AscPullReqType::AccountInfo(i) => {
                    i.target_type == HashType::Block && hash == i.target.into()
                }
                AscPullReqType::Frontiers(_) => false,
            },
            Message::ConfirmAck(i) => i.vote().hashes.contains(&hash),
            Message::ConfirmReq(i) => i.roots_hashes().iter().any(|(h, _)| *h == hash),
            _ => false,
        }
    }

    fn include_account(&self, message: &RecordedMessage) -> bool {
        let Some(account) = self.account else {
            return true;
        };

        match &message.message {
            Message::Publish(i) => i.block.account_field() == Some(account),
            Message::AscPullAck(ack) => match &ack.pull_type {
                AscPullAckType::Blocks(i) => i
                    .blocks()
                    .iter()
                    .any(|b| b.account_field() == Some(account)),
                AscPullAckType::AccountInfo(i) => i.account == account,
                AscPullAckType::Frontiers(i) => i.iter().any(|f| f.account == account),
            },
            Message::AscPullReq(req) => match &req.req_type {
                AscPullReqType::Blocks(i) => {
                    i.start_type == HashType::Account && account == i.start.into()
                }
                AscPullReqType::AccountInfo(i) => {
                    i.target_type == HashType::Account && account == i.target.into()
                }
                AscPullReqType::Frontiers(i) => i.start == account,
            },
            Message::BulkPullAccount(i) => i.account == account,
            Message::ConfirmAck(ack) => ack.vote().voter == account.into(),
            Message::FrontierReq(i) => i.start == account,
            _ => false,
        }
    }
}

#[derive(Default)]
pub(crate) struct FilterCounts {
    pub by_type_direction: HashMap<MessageType, (usize, usize)>,
}

#[derive(Default)]
pub(crate) struct MessageCollection {
    all_messages: Vec<RecordedMessage>,
    filtered: Vec<RecordedMessage>,
    filter: MessageFilter,
    counts: FilterCounts,
}

impl MessageCollection {
    pub fn get(&self, index: usize) -> Option<RecordedMessage> {
        self.filtered.get(index).cloned()
    }

    pub fn len(&self) -> usize {
        self.filtered.len()
    }

    pub fn add(&mut self, message: RecordedMessage) {
        if self.filter.include(&message) {
            self.filtered.push(message.clone());
        }
        if self.filter.include_channel(&message) && self.filter.include_message_content(&message) {
            let entry = self
                .counts
                .by_type_direction
                .entry(message.message.message_type())
                .or_default();
            match message.direction {
                ChannelDirection::Inbound => entry.0 += 1,
                ChannelDirection::Outbound => entry.1 += 1,
            }
        }
        self.all_messages.push(message);
    }

    pub fn clear(&mut self) {
        self.all_messages.clear();
        self.filtered.clear();
    }

    #[cfg(test)]
    pub fn current_filter(&self) -> &MessageFilter {
        &self.filter
    }

    pub fn counts(&self) -> &FilterCounts {
        &self.counts
    }

    pub fn filter_channel(&mut self, channel_id: Option<ChannelId>) {
        self.set_filter(self.filter.with_channel(channel_id))
    }

    pub fn filter_type_direction_pairs(
        &mut self,
        pairs: impl IntoIterator<Item = (MessageType, ChannelDirection)>,
    ) {
        let pairs = pairs.into_iter().collect();
        self.set_filter(self.filter.with_type_direction_pairs(pairs));
    }

    pub fn filter_hash(&mut self, hash: Option<BlockHash>) {
        self.set_filter(self.filter.with_hash(hash));
    }

    pub fn filter_account(&mut self, hash: Option<Account>) {
        self.set_filter(self.filter.with_account(hash));
    }

    fn set_filter(&mut self, filter: MessageFilter) {
        self.filter = filter;
        self.filtered = self
            .all_messages
            .iter()
            .filter(|m| self.filter.include(m))
            .cloned()
            .collect();
        self.counts = FilterCounts::default();

        for m in self
            .all_messages
            .iter()
            .filter(|m| self.filter.include_channel(m) && self.filter.include_message_content(m))
        {
            let entry = self
                .counts
                .by_type_direction
                .entry(m.message.message_type())
                .or_default();
            match m.direction {
                ChannelDirection::Inbound => entry.0 += 1,
                ChannelDirection::Outbound => entry.1 += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let collection = MessageCollection::default();
        assert_eq!(collection.len(), 0);
        assert!(collection.get(0).is_none());
    }

    #[test]
    fn add_message() {
        let mut collection = MessageCollection::default();
        collection.add(RecordedMessage::new_test_instance());
        assert_eq!(collection.len(), 1);
        assert!(collection.get(0).is_some());
    }

    #[test]
    fn clear() {
        let mut collection = MessageCollection::default();
        collection.add(RecordedMessage::new_test_instance());

        collection.clear();

        assert_eq!(collection.len(), 0);
        assert_eq!(collection.all_messages.len(), 0);
        assert_eq!(collection.filtered.len(), 0);
    }

    #[test]
    fn filter() {
        let mut collection = MessageCollection::default();

        let channel_id = ChannelId::from(2);
        let another_channel = ChannelId::from(3);

        let message1 = RecordedMessage {
            channel_id: another_channel,
            ..RecordedMessage::new_test_instance()
        };
        let message2 = RecordedMessage {
            channel_id,
            ..RecordedMessage::new_test_instance()
        };
        let message3 = RecordedMessage {
            channel_id: another_channel,
            ..RecordedMessage::new_test_instance()
        };
        let message4 = RecordedMessage {
            channel_id,
            ..RecordedMessage::new_test_instance()
        };
        collection.add(message1);
        collection.add(message2);
        collection.add(message3);
        collection.add(message4);

        collection.set_filter(MessageFilter::channel(channel_id));

        assert_eq!(collection.len(), 2);
        assert!(collection.get(0).is_some());
        assert!(collection.get(1).is_some());
        assert!(collection.get(2).is_none());
    }

    #[test]
    fn filter_by_type_direction_pair() {
        let mut collection = MessageCollection::default();
        collection.add(inbound(Message::BulkPush));
        collection.add(outbound(Message::BulkPush));
        collection.add(outbound(Message::BulkPush));
        collection.add(inbound(Message::TelemetryReq));

        collection.filter_type_direction_pairs([]);
        assert_eq!(collection.len(), 4, "empty pairs shows all");

        collection
            .filter_type_direction_pairs([(MessageType::BulkPush, ChannelDirection::Inbound)]);
        assert_eq!(collection.len(), 1);
        assert_eq!(
            collection.get(0).unwrap().direction,
            ChannelDirection::Inbound
        );

        collection.filter_type_direction_pairs([
            (MessageType::BulkPush, ChannelDirection::Outbound),
            (MessageType::TelemetryReq, ChannelDirection::Inbound),
        ]);
        assert_eq!(collection.len(), 3);
    }

    #[test]
    fn counts_ignore_type_direction_filter() {
        let mut collection = MessageCollection::default();
        collection.add(inbound(Message::BulkPush));
        collection.add(inbound(Message::BulkPush));
        collection.add(inbound(Message::TelemetryReq));
        collection.add(outbound(Message::BulkPush));
        collection.add(outbound(Message::TelemetryReq));
        collection.add(outbound(Message::TelemetryReq));

        let counts = collection.counts();
        assert_eq!(
            counts.by_type_direction.get(&MessageType::BulkPush),
            Some(&(2, 1))
        );
        assert_eq!(
            counts.by_type_direction.get(&MessageType::TelemetryReq),
            Some(&(1, 2))
        );

        collection
            .filter_type_direction_pairs([(MessageType::BulkPush, ChannelDirection::Inbound)]);
        let counts = collection.counts();
        assert_eq!(
            counts.by_type_direction.get(&MessageType::BulkPush),
            Some(&(2, 1)),
            "counts ignore the active filter"
        );
        assert_eq!(
            counts.by_type_direction.get(&MessageType::TelemetryReq),
            Some(&(1, 2))
        );
    }

    /* Test helpers */

    fn inbound(message: Message) -> RecordedMessage {
        RecordedMessage {
            message,
            direction: ChannelDirection::Inbound,
            ..RecordedMessage::new_test_instance()
        }
    }

    fn outbound(message: Message) -> RecordedMessage {
        RecordedMessage {
            message,
            direction: ChannelDirection::Outbound,
            ..RecordedMessage::new_test_instance()
        }
    }
}
