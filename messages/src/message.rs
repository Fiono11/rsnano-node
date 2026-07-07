use super::*;
use bitvec::prelude::BitArray;
use rsnano_types::{DeserializationError, ProtocolInfo};
use rsnano_utils::stats::DetailType;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Keepalive(Keepalive),
    Publish(Publish),
    AscPullAck(AscPullAck),
    AscPullReq(AscPullReq),
    BulkPull(BulkPull),
    BulkPullAccount(BulkPullAccount),
    BulkPush,
    ConfirmAck(ConfirmAck),
    ConfirmReq(ConfirmReq),
    FrontierReq(FrontierReq),
    Handshake(Handshake),
    TelemetryAck(TelemetryAck),
    TelemetryReq,
}

pub trait MessageVariant {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        Default::default()
    }
}

impl From<&Message> for DetailType {
    fn from(value: &Message) -> Self {
        value.message_type().into()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ParseMessageError {
    Other(String),
    InvalidHeader,
    InvalidMessageType,
    InvalidMessage(MessageType),
    InvalidNetwork,
    OutdatedVersion,
    DuplicatePublishMessage,
    DuplicateConfirmAckMessage,
    MessageSizeTooBig,
    Stopped,
}

impl From<&ParseMessageError> for DetailType {
    fn from(status: &ParseMessageError) -> Self {
        match status {
            ParseMessageError::Other(_) | ParseMessageError::Stopped => Self::All,
            ParseMessageError::InvalidHeader => Self::InvalidHeader,
            ParseMessageError::InvalidMessageType => Self::InvalidMessageType,
            ParseMessageError::InvalidMessage(MessageType::Keepalive) => {
                Self::InvalidKeepaliveMessage
            }
            ParseMessageError::InvalidMessage(MessageType::Publish) => Self::InvalidPublishMessage,
            ParseMessageError::InvalidMessage(MessageType::ConfirmReq) => {
                Self::InvalidConfirmReqMessage
            }
            ParseMessageError::InvalidMessage(MessageType::ConfirmAck) => {
                Self::InvalidConfirmAckMessage
            }
            ParseMessageError::InvalidMessage(MessageType::Handshake) => {
                Self::InvalidNodeIdHandshakeMessage
            }
            ParseMessageError::InvalidMessage(MessageType::TelemetryReq) => {
                Self::InvalidTelemetryReqMessage
            }
            ParseMessageError::InvalidMessage(MessageType::TelemetryAck) => {
                Self::InvalidTelemetryAckMessage
            }
            ParseMessageError::InvalidMessage(MessageType::BulkPull) => {
                Self::InvalidBulkPullMessage
            }
            ParseMessageError::InvalidMessage(MessageType::BulkPullAccount) => {
                Self::InvalidBulkPullAccountMessage
            }
            ParseMessageError::InvalidMessage(MessageType::FrontierReq) => {
                Self::InvalidFrontierReqMessage
            }
            ParseMessageError::InvalidMessage(MessageType::AscPullReq) => {
                Self::InvalidAscPullReqMessage
            }
            ParseMessageError::InvalidMessage(MessageType::AscPullAck) => {
                Self::InvalidAscPullAckMessage
            }
            ParseMessageError::InvalidMessage(MessageType::BulkPush) => Self::InvalidMessageType,
            ParseMessageError::InvalidMessage(MessageType::Invalid)
            | ParseMessageError::InvalidMessage(MessageType::NotAType) => Self::InvalidMessageType,
            ParseMessageError::InvalidNetwork => Self::InvalidNetwork,
            ParseMessageError::OutdatedVersion => Self::OutdatedVersion,
            ParseMessageError::DuplicatePublishMessage => Self::DuplicatePublishMessage,
            ParseMessageError::DuplicateConfirmAckMessage => Self::DuplicateConfirmAckMessage,
            ParseMessageError::MessageSizeTooBig => Self::MessageSizeTooBig,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeserializedMessage {
    pub message: Message,
    pub protocol: ProtocolInfo,
}

impl DeserializedMessage {
    pub fn new(message: Message, protocol: ProtocolInfo) -> Self {
        Self { message, protocol }
    }
}

impl Message {
    pub const MAX_MESSAGE_SIZE: usize = 1024 * 65;

    pub fn message_type(&self) -> MessageType {
        match &self {
            Message::Keepalive(_) => MessageType::Keepalive,
            Message::Publish(_) => MessageType::Publish,
            Message::AscPullAck(_) => MessageType::AscPullAck,
            Message::AscPullReq(_) => MessageType::AscPullReq,
            Message::BulkPull(_) => MessageType::BulkPull,
            Message::BulkPullAccount(_) => MessageType::BulkPullAccount,
            Message::BulkPush => MessageType::BulkPush,
            Message::ConfirmAck(_) => MessageType::ConfirmAck,
            Message::ConfirmReq(_) => MessageType::ConfirmReq,
            Message::FrontierReq(_) => MessageType::FrontierReq,
            Message::Handshake(_) => MessageType::Handshake,
            Message::TelemetryAck(_) => MessageType::TelemetryAck,
            Message::TelemetryReq => MessageType::TelemetryReq,
        }
    }

    pub fn as_message_variant(&self) -> Option<&dyn MessageVariant> {
        match &self {
            Message::Keepalive(x) => Some(x),
            Message::Publish(x) => Some(x),
            Message::AscPullAck(x) => Some(x),
            Message::AscPullReq(x) => Some(x),
            Message::BulkPull(x) => Some(x),
            Message::BulkPullAccount(x) => Some(x),
            Message::ConfirmAck(x) => Some(x),
            Message::ConfirmReq(x) => Some(x),
            Message::FrontierReq(x) => Some(x),
            Message::Handshake(x) => Some(x),
            Message::TelemetryAck(x) => Some(x),
            _ => None,
        }
    }

    pub fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        match self.as_message_variant() {
            Some(variant) => variant.header_extensions(payload_len),
            None => Default::default(),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        match self {
            Message::Keepalive(m) => m.serialize(writer),
            Message::Publish(m) => m.serialize(writer),
            Message::AscPullAck(m) => m.serialize(writer),
            Message::AscPullReq(m) => m.serialize(writer),
            Message::BulkPull(m) => m.serialize(writer),
            Message::BulkPullAccount(m) => m.serialize(writer),
            Message::ConfirmAck(m) => m.serialize(writer),
            Message::ConfirmReq(m) => m.serialize(writer),
            Message::FrontierReq(m) => m.serialize(writer),
            Message::Handshake(m) => m.serialize(writer),
            Message::TelemetryAck(m) => m.serialize(writer),
            Message::BulkPush | Message::TelemetryReq => Ok(()),
        }
    }

    pub fn deserialize(
        payload: &[u8],
        header: &MessageHeader,
        digest: u128,
    ) -> Result<Self, DeserializationError> {
        let msg = match header.message_type {
            MessageType::Keepalive => Message::Keepalive(Keepalive::deserialize(payload)?),
            MessageType::Publish => {
                Message::Publish(Publish::deserialize(payload, header.extensions, digest)?)
            }
            MessageType::AscPullAck => Message::AscPullAck(AscPullAck::deserialize(payload)?),
            MessageType::AscPullReq => Message::AscPullReq(AscPullReq::deserialize(payload)?),
            MessageType::BulkPull => {
                Message::BulkPull(BulkPull::deserialize(payload, header.extensions)?)
            }
            MessageType::BulkPullAccount => {
                Message::BulkPullAccount(BulkPullAccount::deserialize(payload)?)
            }
            MessageType::BulkPush => Message::BulkPush,
            MessageType::ConfirmAck => {
                Message::ConfirmAck(ConfirmAck::deserialize(payload, header.extensions, digest)?)
            }
            MessageType::ConfirmReq => {
                Message::ConfirmReq(ConfirmReq::deserialize(payload, header.extensions)?)
            }
            MessageType::FrontierReq => {
                Message::FrontierReq(FrontierReq::deserialize(payload, header.extensions)?)
            }
            MessageType::Handshake => {
                Message::Handshake(Handshake::deserialize(payload, header.extensions)?)
            }
            MessageType::TelemetryAck => {
                Message::TelemetryAck(TelemetryAck::deserialize(payload, header.extensions)?)
            }
            MessageType::TelemetryReq => Message::TelemetryReq,
            MessageType::Invalid | MessageType::NotAType => {
                return Err(DeserializationError::InvalidData);
            }
        };

        Ok(msg)
    }

    pub fn is_obsolete(&self) -> bool {
        matches!(
            self.message_type(),
            MessageType::BulkPull
                | MessageType::BulkPullAccount
                | MessageType::BulkPush
                | MessageType::TelemetryReq
        )
    }
}

pub fn validate_header(
    header: &MessageHeader,
    expected_protocol: &ProtocolInfo,
) -> Result<(), ParseMessageError> {
    if header.protocol.network != expected_protocol.network {
        Err(ParseMessageError::InvalidNetwork)
    } else if header.protocol.version_using < expected_protocol.version_min {
        Err(ParseMessageError::OutdatedVersion)
    } else if !header.is_valid_message_type() {
        Err(ParseMessageError::InvalidHeader)
    } else if header.payload_length() > Message::MAX_MESSAGE_SIZE {
        Err(ParseMessageError::MessageSizeTooBig)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{TestBlockBuilder, Vote};

    #[test]
    fn exact_confirm_ack() {
        let message = Message::ConfirmAck(ConfirmAck::new_with_own_vote(Vote::new_test_instance()));
        assert_deserializable(&message);
    }

    #[test]
    fn exact_confirm_req() {
        let message = Message::ConfirmReq(ConfirmReq::new_test_instance());
        assert_deserializable(&message);
    }

    #[test]
    fn exact_publish() {
        let block = TestBlockBuilder::legacy_send().build();
        let message = Message::Publish(Publish::new_from_originator(block));
        assert_deserializable(&message);
    }

    #[test]
    fn exact_keepalive() {
        assert_deserializable(&Message::Keepalive(Keepalive::default()));
    }

    #[test]
    fn exact_frontier_req() {
        let message = Message::FrontierReq(FrontierReq::new_test_instance());
        assert_deserializable(&message);
    }

    #[test]
    fn exact_telemetry_req() {
        assert_deserializable(&Message::TelemetryReq);
    }

    #[test]
    fn exact_telemetry_ack() {
        let mut data = TelemetryData::default();
        data.unknown_data.push(0xFF);
        assert_deserializable(&Message::TelemetryAck(TelemetryAck(Some(data))));
    }

    #[test]
    fn exact_bulk_pull() {
        let message = Message::BulkPull(BulkPull::new_test_instance());
        assert_deserializable(&message);
    }

    #[test]
    fn exact_bulk_pull_account() {
        let message = Message::BulkPullAccount(BulkPullAccount::new_test_instance());
        assert_deserializable(&message);
    }

    #[test]
    fn exact_bulk_push() {
        assert_deserializable(&Message::BulkPush);
    }

    #[test]
    fn exact_node_id_handshake() {
        let message = Message::Handshake(Handshake {
            query: Some(HandshakeQuery { cookie: [1; 32] }),
            response: None,
            is_v2: true,
        });
        assert_deserializable(&message);
    }

    #[test]
    fn exact_asc_pull_req() {
        let message = Message::AscPullReq(AscPullReq {
            req_type: AscPullReqType::AccountInfo(AccountInfoReqPayload::new_test_instance()),
            id: 7,
        });
        assert_deserializable(&message);
    }

    #[test]
    fn exact_asc_pull_ack() {
        let message = Message::AscPullAck(AscPullAck {
            id: 7,
            pull_type: AscPullAckType::AccountInfo(AccountInfoAckPayload::new_test_instance()),
        });
        assert_deserializable(&message);
    }
}
