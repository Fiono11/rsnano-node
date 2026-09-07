use std::sync::{Arc, Mutex, RwLock, Weak};

use tracing::{debug, warn};

use rsnano_messages::*;
use rsnano_network::{
    Channel, ChannelDirection, ChannelMode, DataReceiver, Network, ReceiveResult, TrafficType,
};
use rsnano_types::{NodeId, ProtocolInfo};
use rsnano_utils::stats::{DetailType, Direction, StatType, Stats};

use crate::{HandshakeProcess, HandshakeStatus, LatestKeepalives};

pub struct NanoDataReceiver {
    channel: Arc<Channel>,
    handshake_process: HandshakeProcess,
    serializer: MessageSerializer,
    message_deserializer: MessageDeserializer,
    try_enqueue: Arc<dyn Fn(Message, Arc<Channel>) -> bool + Send + Sync>,
    latest_keepalives: Arc<Mutex<LatestKeepalives>>,
    stats: Arc<Stats>,
    network: Weak<RwLock<Network>>,
    first_message: bool,
    node_id: NodeId,
    retry_enqueue: Option<Message>,
}

impl NanoDataReceiver {
    pub fn new(
        channel: Arc<Channel>,
        handshake_process: HandshakeProcess,
        message_deserializer: MessageDeserializer,
        try_enqueue: Arc<dyn Fn(Message, Arc<Channel>) -> bool + Send + Sync>,
        latest_keepalives: Arc<Mutex<LatestKeepalives>>,
        stats: Arc<Stats>,
        network: Weak<RwLock<Network>>,
        protocol: ProtocolInfo,
    ) -> Self {
        Self {
            channel,
            handshake_process,
            serializer: MessageSerializer::new_with_buffer_size(protocol, 512),
            message_deserializer,
            try_enqueue,
            latest_keepalives,
            stats,
            network,
            first_message: true,
            node_id: NodeId::ZERO,
            retry_enqueue: None,
        }
    }

    pub fn ensure_handshake(&mut self) {
        if self.channel.direction() == ChannelDirection::Outbound {
            self.initiate_handshake();
        }
    }

    fn initiate_handshake(&mut self) {
        let peer = self.channel.peer_addr();
        let result = self.handshake_process.initiate_handshake(peer);

        match result {
            Ok(handshake) => {
                let data = self.serializer.serialize(&Message::Handshake(handshake));

                debug!("Initiating handshake query ({})", peer);
                let enqueued = self.channel.send(data, TrafficType::Generic);
                if !(enqueued) {
                    warn!(%peer, "Could not send handshake");
                    self.channel.close();
                }
            }
            Err(e) => {
                warn!("Could not initiate handshake: {:?}", e);
                self.channel.close();
            }
        }
    }

    fn queue_established(&mut self, message: Message) -> ReceiveResult {
        let enqueued = self.try_enqueue(message.clone());
        if enqueued {
            ReceiveResult::Continue
        } else {
            debug_assert!(self.retry_enqueue.is_none());
            self.retry_enqueue = Some(message);
            ReceiveResult::Pause
        }
    }

    fn try_enqueue(&self, message: Message) -> bool {
        (self.try_enqueue)(message, self.channel.clone())
    }

    fn set_last_keepalive(&self, keepalive: Keepalive) {
        self.latest_keepalives
            .lock()
            .unwrap()
            .insert(self.channel.channel_id(), keepalive);
    }

    fn process_established(&mut self, message: Message) -> ReceiveResult {
        if message.is_obsolete() {
            // TODO: Ban the peer?
            debug!(message_type = ?message.message_type(), "Received an obsolete message");
            return ReceiveResult::Continue;
        }

        if let Message::Keepalive(keepalive) = &message {
            self.set_last_keepalive(keepalive.clone());
        }

        self.queue_established(message)
    }

    fn to_established_connection(&self, node_id: &NodeId) -> bool {
        if self.channel.mode() != ChannelMode::Handshake {
            return false;
        }

        let Some(network) = self.network.upgrade() else {
            return false;
        };

        let result = network
            .read()
            .unwrap()
            .upgrade_to_established_connection(self.channel.channel_id(), *node_id);

        if result.is_some() {
            self.stats
                .inc(StatType::TcpChannels, DetailType::ChannelAccepted);

            debug!(
                "Switched to established mode (addr: {}, node_id: {})",
                self.channel.peer_addr(),
                node_id
            );
            true
        } else {
            debug!(
                channel_id = ?self.channel.channel_id(),
                peer = %self.channel.peer_addr(),
                %node_id,
                "Could not upgrade channel to established connection, because another channel for the same node ID was found",
            );
            false
        }
    }

    fn process_message(&mut self, message: Message) -> ReceiveResult {
        self.stats.inc_dir(
            StatType::TcpServer,
            DetailType::from(message.message_type()),
            Direction::In,
        );

        /*
         * The channel initially starts in handshake state, where it waits for either a handshake message.
         * If the server receives a handshake (and it is successfully validated) it will switch to an established mode.
         * In established mode messages are deserialized and queued for further processing.
         * In established mode any legacy bootstrap requests are ignored.
         */
        if self.channel.mode() == ChannelMode::Handshake {
            let (mut status, response) = match &message {
                Message::Handshake(payload) => {
                    let log_type = match (payload.query.is_some(), payload.response.is_some()) {
                        (true, true) => "query + response",
                        (true, false) => "query",
                        (false, true) => "response",
                        (false, false) => "none",
                    };
                    debug!(
                        "Handshake message received: {} ({})",
                        log_type,
                        self.channel.peer_addr()
                    );

                    match self
                        .handshake_process
                        .process_handshake(payload, self.channel.peer_addr())
                    {
                        Ok((their_node_id, response)) => match their_node_id {
                            Some(node_id) => (HandshakeStatus::Completed(node_id), response),
                            None => (HandshakeStatus::Handshake, response),
                        },
                        Err(e) => {
                            if matches!(e, crate::HandshakeError::OwnNodeId) {
                                warn!(
                                    "This node tried to connect to itself. Closing channel ({})",
                                    self.channel.peer_addr()
                                );
                            }
                            debug!(
                                peer = %self.channel.peer_addr(),
                                error = ?e,
                                "Invalid handshake response received"
                            );
                            (HandshakeStatus::Abort, None)
                        }
                    }
                }

                _ => (HandshakeStatus::Abort, None),
            };

            if let Some(response) = response {
                debug!("Responding to handshake ({})", self.channel.peer_addr());
                let buffer = self.serializer.serialize(&Message::Handshake(response));

                let enqueued = self.channel.send(buffer, TrafficType::Generic);
                if !enqueued {
                    warn!(peer = %self.channel.peer_addr(), "Error sending handshake response");
                    status = HandshakeStatus::Abort;
                }
            }

            match status {
                HandshakeStatus::Abort | HandshakeStatus::AbortOwnNodeId => {
                    self.stats.inc_dir(
                        StatType::TcpServer,
                        DetailType::HandshakeAbort,
                        Direction::In,
                    );
                    debug!(
                        "Aborting handshake: {:?} ({})",
                        message.message_type(),
                        self.channel.peer_addr()
                    );
                    if matches!(status, HandshakeStatus::AbortOwnNodeId)
                        && let Some(peering_addr) = self.channel.peering_addr()
                        && let Some(network) = self.network.upgrade()
                    {
                        network.write().unwrap().perma_ban(peering_addr);
                    }
                    return ReceiveResult::Abort;
                }
                HandshakeStatus::Handshake => {
                    return ReceiveResult::Continue; // Continue handshake
                }
                HandshakeStatus::Completed(node_id) => {
                    self.node_id = node_id;
                    // Wait until send queue is empty for the handshake to complete
                    return ReceiveResult::Pause;
                }
            }
        } else if self.channel.mode() == ChannelMode::Established {
            return self.process_established(message);
        }

        debug_assert!(false);
        ReceiveResult::Abort
    }
}

impl DataReceiver for NanoDataReceiver {
    fn receive(&mut self, data: &[u8]) -> ReceiveResult {
        self.message_deserializer.push(data);
        while let Some(result) = self.message_deserializer.try_deserialize() {
            let result = match result {
                Ok(msg) => {
                    if self.first_message {
                        // TODO: if version using changes => peer misbehaved!
                        self.channel
                            .set_protocol_version(msg.protocol.version_using);
                        self.first_message = false;
                    }
                    self.process_message(msg.message)
                }
                Err(ParseMessageError::DuplicatePublishMessage) => {
                    // Avoid too much noise about `duplicate_publish_message` errors
                    self.stats.inc_dir(
                        StatType::Filter,
                        DetailType::DuplicatePublishMessage,
                        Direction::In,
                    );
                    ReceiveResult::Continue
                }
                Err(ParseMessageError::DuplicateConfirmAckMessage) => {
                    self.stats.inc_dir(
                        StatType::Filter,
                        DetailType::DuplicateConfirmAckMessage,
                        Direction::In,
                    );
                    ReceiveResult::Continue
                }
                Err(e) => {
                    // IO error or critical error when deserializing message
                    self.stats
                        .inc_dir(StatType::Error, DetailType::from(&e), Direction::In);
                    debug!(
                        "Error reading message: {:?} ({})",
                        e,
                        self.channel.peer_addr()
                    );
                    ReceiveResult::Abort
                }
            };

            if !matches!(result, ReceiveResult::Continue) {
                return result;
            }
        }

        ReceiveResult::Continue
    }

    fn try_unpause(&mut self) -> ReceiveResult {
        let mode = self.channel.mode();
        let result = match mode {
            ChannelMode::Handshake => {
                // Paused during handshake

                // Wait until all outbound messages are processed.
                // This is needed for the handshake because the channel can't be upgraded to
                // an established channel unless the handshake response is actually sent out
                if self.channel.queue_len() > 0 {
                    return ReceiveResult::Pause;
                }

                if !self.to_established_connection(&self.node_id) {
                    self.stats.inc_dir(
                        StatType::TcpServer,
                        DetailType::HandshakeError,
                        Direction::In,
                    );
                    debug!(
                        "Error switching to established mode ({})",
                        self.channel.peer_addr()
                    );
                    return ReceiveResult::Abort;
                }

                ReceiveResult::Continue
            }
            ChannelMode::Established => {
                let message = self.retry_enqueue.clone();
                match message {
                    Some(message) => {
                        if self.try_enqueue(message) {
                            self.retry_enqueue = None;
                            ReceiveResult::Continue
                        } else {
                            ReceiveResult::Pause
                        }
                    }
                    None => ReceiveResult::Continue,
                }
            }
        };
        if matches!(result, ReceiveResult::Continue) {
            // The read that paused us may already contain more complete messages. Drain
            // them now, rather than waiting for another read from an otherwise idle peer.
            self.receive(&[])
        } else {
            result
        }
    }
}

impl Drop for NanoDataReceiver {
    fn drop(&mut self) {
        self.channel.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn resumes_buffered_messages_after_backpressure_without_replaying_old_messages() {
        let channel = Arc::new(Channel::new_test_instance());
        channel.set_mode(ChannelMode::Established);
        let accepting = Arc::new(AtomicBool::new(false));
        let accepted = Arc::new(Mutex::new(Vec::new()));
        let enqueue = {
            let accepting = accepting.clone();
            let accepted = accepted.clone();
            Arc::new(move |message, _: Arc<Channel>| {
                if accepting.load(Ordering::SeqCst) {
                    accepted.lock().unwrap().push(message);
                    true
                } else {
                    false
                }
            })
        };
        let protocol = ProtocolInfo::default();
        let mut receiver = NanoDataReceiver::new(
            channel,
            HandshakeProcess::new(
                1.into(),
                rsnano_types::PrivateKey::from(1),
                Arc::new(crate::SynCookies::new(10)),
                Arc::new(crate::HandshakeStats::default()),
            ),
            MessageDeserializer::new(protocol),
            enqueue,
            Arc::new(Mutex::new(LatestKeepalives::default())),
            Arc::new(Stats::default()),
            Weak::new(),
            protocol,
        );
        let message = Message::Keepalive(Keepalive::default());
        let mut serializer = MessageSerializer::new(protocol);
        let bytes = serializer.serialize(&message).to_vec();
        let mut two_messages = bytes.clone();
        two_messages.extend_from_slice(&bytes);
        assert!(matches!(
            receiver.receive(&two_messages),
            ReceiveResult::Pause
        ));
        assert!(matches!(receiver.try_unpause(), ReceiveResult::Pause));
        accepting.store(true, Ordering::SeqCst);
        assert!(matches!(receiver.try_unpause(), ReceiveResult::Continue));
        assert!(receiver.retry_enqueue.is_none());
        assert_eq!(
            accepted.lock().unwrap().len(),
            2,
            "drain the already buffered second message"
        );
        assert!(matches!(receiver.try_unpause(), ReceiveResult::Continue));
        assert_eq!(
            accepted.lock().unwrap().len(),
            2,
            "do not enqueue the old message again"
        );

        accepting.store(false, Ordering::SeqCst);
        assert!(matches!(receiver.receive(&bytes), ReceiveResult::Pause));
        accepting.store(true, Ordering::SeqCst);
        assert!(matches!(receiver.try_unpause(), ReceiveResult::Continue));
        assert_eq!(accepted.lock().unwrap().len(), 3);
    }
}
