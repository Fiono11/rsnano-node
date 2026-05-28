use std::sync::{Mutex, Weak};

use rsnano_network::{ChannelEvent, TrafficType};
use rsnano_utils::EventHandler;

use crate::{
    representatives::RepCrawler,
    transport::{MessageSender, keepalive::KeepaliveMessageFactory},
};

/// Asks the rep crawler to prioritise querying a newly established channel.
pub(crate) struct RepCrawlerOnEstablishedHandler {
    rep_crawler: Weak<RepCrawler>,
}

impl RepCrawlerOnEstablishedHandler {
    pub fn new(rep_crawler: Weak<RepCrawler>) -> Self {
        Self { rep_crawler }
    }
}

impl EventHandler<ChannelEvent> for RepCrawlerOnEstablishedHandler {
    fn handle(&self, event: &ChannelEvent) {
        if let ChannelEvent::Established(channel) = event {
            if let Some(crawler) = self.rep_crawler.upgrade() {
                crawler.query_with_priority(channel.clone());
            }
        }
    }
}

/// Sends an initial keepalive to a newly established channel.
pub(crate) struct KeepaliveOnEstablishedHandler {
    keepalive_factory: Weak<KeepaliveMessageFactory>,
    message_sender: Weak<Mutex<MessageSender>>,
}

impl KeepaliveOnEstablishedHandler {
    pub fn new(
        keepalive_factory: Weak<KeepaliveMessageFactory>,
        message_sender: Weak<Mutex<MessageSender>>,
    ) -> Self {
        Self {
            keepalive_factory,
            message_sender,
        }
    }
}

impl EventHandler<ChannelEvent> for KeepaliveOnEstablishedHandler {
    fn handle(&self, event: &ChannelEvent) {
        if let ChannelEvent::Established(channel) = event {
            let Some(factory) = self.keepalive_factory.upgrade() else {
                return;
            };
            let Some(sender) = self.message_sender.upgrade() else {
                return;
            };
            let keepalive = factory.create_keepalive_self();
            sender
                .lock()
                .unwrap()
                .try_send(channel, &keepalive, TrafficType::Keepalive);
        }
    }
}
