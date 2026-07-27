use std::{
    fmt::{Debug, Display},
    io::Read,
};

use bitvec::prelude::*;
use num_traits::FromPrimitive;

use rsnano_types::{DeserializationError, NetworkType, ProtocolInfo, read_u8};
use rsnano_utils::stats::DetailType;

use super::*;

/// Message types are serialized to the network and existing values must thus never change as
/// types are added, removed and reordered in the enum.
#[repr(u8)]
#[derive(FromPrimitive, Clone, Copy, PartialEq, Eq, Hash, EnumCount, EnumIter)]
pub enum MessageType {
    Invalid = 0x0,
    NotAType = 0x1,
    Keepalive = 0x2,
    Publish = 0x3,
    ConfirmReq = 0x4,
    ConfirmAck = 0x5,
    BulkPull = 0x6,
    BulkPush = 0x7,
    FrontierReq = 0x8,
    /* deleted 0x9 */
    Handshake = 0x0a,
    BulkPullAccount = 0x0b,
    TelemetryReq = 0x0c,
    TelemetryAck = 0x0d,
    AscPullReq = 0x0e,
    AscPullAck = 0x0f,
    RaiVote = 0x10,
    RaiPendingReport = 0x11,
    RaiReconciliation = 0x12,
}

impl MessageType {
    pub const RAI_VOTE_ID: u8 = 0x10;
    pub const RAI_PENDING_REPORT_ID: u8 = 0x11;

    pub fn as_str(&self) -> &'static str {
        match self {
            MessageType::Invalid => "invalid",
            MessageType::NotAType => "not_a_type",
            MessageType::Keepalive => "keepalive",
            MessageType::Publish => "publish",
            MessageType::ConfirmReq => "confirm_req",
            MessageType::ConfirmAck => "confirm_ack",
            MessageType::BulkPull => "bulk_pull",
            MessageType::BulkPush => "bulk_push",
            MessageType::FrontierReq => "frontier_req",
            MessageType::Handshake => "node_id_handshake",
            MessageType::BulkPullAccount => "bulk_pull_account",
            MessageType::TelemetryReq => "telemetry_req",
            MessageType::TelemetryAck => "telemetry_ack",
            MessageType::AscPullReq => "asc_pull_req",
            MessageType::AscPullAck => "asc_pull_ack",
            MessageType::RaiVote => "rai_vote",
            MessageType::RaiPendingReport => "rai_pending_report",
            MessageType::RaiReconciliation => "rai_reconciliation",
        }
    }

    pub const fn max_id() -> usize {
        Self::RaiReconciliation as usize
    }

    pub const fn requires_rai_protocol(self) -> bool {
        matches!(
            self,
            Self::RaiVote | Self::RaiPendingReport | Self::RaiReconciliation
        )
    }

    pub const fn is_supported_by_protocol(self, protocol_version: u8) -> bool {
        !self.requires_rai_protocol() || protocol_version >= ProtocolInfo::RAI_PROTOCOL_VERSION
    }

    pub const fn is_supported_by_local_node(self) -> bool {
        #[cfg(feature = "rai_protocol")]
        {
            true
        }
        #[cfg(not(feature = "rai_protocol"))]
        {
            !self.requires_rai_protocol()
        }
    }
}

impl Debug for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/*
 * Common Header Binary Format:
 * [2 bytes] Network (big endian)
 * [1 byte] Maximum protocol version
 * [1 byte] Protocol version currently in use
 * [1 byte] Minimum protocol version
 * [1 byte] Message type
 * [2 bytes] Extensions (message-specific flags and properties)
 *
 * Notes:
 * - The structure and bit usage of the `extensions` field vary by message type.
 */
#[derive(Clone, PartialEq, Eq)]
pub struct MessageHeader {
    pub message_type: MessageType,
    pub protocol: ProtocolInfo,
    pub extensions: BitArray<u16>,
}

impl Default for MessageHeader {
    fn default() -> Self {
        Self {
            message_type: MessageType::Invalid,
            protocol: Default::default(),
            extensions: BitArray::ZERO,
        }
    }
}

impl MessageHeader {
    pub const SERIALIZED_SIZE: usize = 8;

    pub fn new(message_type: MessageType, protocol: ProtocolInfo) -> Self {
        Self {
            message_type,
            protocol,
            ..Default::default()
        }
    }

    pub fn set_extension(&mut self, position: usize, value: bool) {
        self.extensions.set(position, value);
    }

    pub fn set_flag(&mut self, flag: u8) {
        // Flags from 8 are block_type & count
        debug_assert!(flag < 8);
        self.extensions.set(flag as usize, true);
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        writer.write_all(&(self.protocol.network as u16).to_be_bytes())?;
        writer.write_all(&[
            self.protocol.version_max,
            self.protocol.version_using,
            self.protocol.version_min,
            self.message_type as u8,
        ])?;
        writer.write_all(&self.extensions.data.to_le_bytes())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<Self, DeserializationError>
    where
        T: Read,
    {
        let mut header = Self::default();
        let mut buffer = [0; 2];

        reader.read_exact(&mut buffer)?;
        header.protocol.network = NetworkType::from_u16(u16::from_be_bytes(buffer))
            .ok_or(DeserializationError::InvalidData)?;

        header.protocol.version_max = read_u8(reader)?;
        header.protocol.version_using = read_u8(reader)?;
        header.protocol.version_min = read_u8(reader)?;
        header.message_type =
            MessageType::from_u8(read_u8(reader)?).ok_or(DeserializationError::InvalidData)?;

        reader.read_exact(&mut buffer)?;
        header.extensions.data = u16::from_le_bytes(buffer);
        Ok(header)
    }

    const BULK_PULL_COUNT_PRESENT_FLAG: usize = 0;

    pub fn bulk_pull_is_count_present(&self) -> bool {
        self.message_type == MessageType::BulkPull
            && self.extensions[Self::BULK_PULL_COUNT_PRESENT_FLAG]
    }

    pub fn payload_length(&self) -> usize {
        match self.message_type {
            MessageType::Keepalive => Keepalive::SERIALIZED_SIZE,
            MessageType::Publish => Publish::serialized_size(self.extensions),
            MessageType::ConfirmReq => ConfirmReq::serialized_size(self.extensions),
            MessageType::ConfirmAck => ConfirmAck::serialized_size(self.extensions),
            MessageType::BulkPull => BulkPull::serialized_size(self.extensions),
            MessageType::BulkPush | MessageType::TelemetryReq => 0,
            MessageType::FrontierReq => FrontierReq::SERIALIZED_SIZE,
            MessageType::Handshake => Handshake::serialized_size(self.extensions),
            MessageType::BulkPullAccount => BulkPullAccount::SERIALIZED_SIZE,
            MessageType::TelemetryAck => TelemetryAck::serialized_size(self.extensions),
            MessageType::AscPullReq => AscPullReq::serialized_size(self.extensions),
            MessageType::AscPullAck => AscPullAck::serialized_size(self.extensions),
            #[cfg(feature = "rai_protocol")]
            MessageType::RaiVote => rsnano_types::RaiVote::SERIALIZED_SIZE,
            #[cfg(not(feature = "rai_protocol"))]
            MessageType::RaiVote => {
                debug_assert!(false);
                0
            }
            #[cfg(feature = "rai_protocol")]
            MessageType::RaiPendingReport => {
                let count = crate::rai::rai_pending_report_count(self.extensions);
                rsnano_types::RaiPendingReport::serialized_size(count)
            }
            #[cfg(feature = "rai_protocol")]
            MessageType::RaiReconciliation => {
                crate::RaiReconciliation::serialized_size(self.extensions)
            }
            #[cfg(not(feature = "rai_protocol"))]
            MessageType::RaiReconciliation => {
                debug_assert!(false);
                0
            }
            #[cfg(not(feature = "rai_protocol"))]
            MessageType::RaiPendingReport => {
                debug_assert!(false);
                0
            }
            MessageType::Invalid | MessageType::NotAType => {
                debug_assert!(false);
                0
            }
        }
    }

    pub fn is_valid_message_type(&self) -> bool {
        !matches!(
            self.message_type,
            MessageType::Invalid | MessageType::NotAType
        )
    }
}

impl Display for MessageHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "NetID: {:04X}({}), ",
            self.protocol.network as u16,
            self.protocol.network.as_str()
        ))?;
        f.write_fmt(format_args!(
            "VerMaxUsingMin: {}/{}/{}, ",
            self.protocol.version_max, self.protocol.version_using, self.protocol.version_min
        ))?;
        f.write_fmt(format_args!(
            "MsgType: {}({}), ",
            self.message_type as u8,
            self.message_type.as_str()
        ))?;
        f.write_fmt(format_args!("Extensions: {:04X}", self.extensions.data))
    }
}

impl Debug for MessageHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self, f)
    }
}

impl From<MessageType> for DetailType {
    fn from(msg: MessageType) -> Self {
        match msg {
            MessageType::Invalid => DetailType::Invalid,
            MessageType::NotAType => DetailType::NotAType,
            MessageType::Keepalive => DetailType::Keepalive,
            MessageType::Publish => DetailType::Publish,
            MessageType::ConfirmReq => DetailType::ConfirmReq,
            MessageType::ConfirmAck => DetailType::ConfirmAck,
            MessageType::BulkPull => DetailType::BulkPull,
            MessageType::BulkPush => DetailType::BulkPush,
            MessageType::FrontierReq => DetailType::FrontierReq,
            MessageType::Handshake => DetailType::NodeIdHandshake,
            MessageType::BulkPullAccount => DetailType::BulkPullAccount,
            MessageType::TelemetryReq => DetailType::TelemetryReq,
            MessageType::TelemetryAck => DetailType::TelemetryAck,
            MessageType::AscPullReq => DetailType::AscPullReq,
            MessageType::AscPullAck => DetailType::AscPullAck,
            #[cfg(feature = "rai_protocol")]
            MessageType::RaiVote => DetailType::RaiVote,
            #[cfg(not(feature = "rai_protocol"))]
            MessageType::RaiVote => DetailType::InvalidMessageType,
            #[cfg(feature = "rai_protocol")]
            MessageType::RaiPendingReport => DetailType::RaiPendingReport,
            #[cfg(feature = "rai_protocol")]
            MessageType::RaiReconciliation => DetailType::RaiVote,
            #[cfg(not(feature = "rai_protocol"))]
            MessageType::RaiPendingReport => DetailType::InvalidMessageType,
            #[cfg(not(feature = "rai_protocol"))]
            MessageType::RaiReconciliation => DetailType::InvalidMessageType,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_header_to_string() {
        assert_eq!(
            test_header().to_string(),
            "NetID: 5241(dev), VerMaxUsingMin: 3/2/1, MsgType: 2(keepalive), Extensions: 000E"
        );
    }

    #[test]
    fn serialize_and_deserialize() {
        let original = test_header();
        let mut buffer = Vec::new();
        original.serialize(&mut buffer).unwrap();

        let deserialized = MessageHeader::deserialize(&mut buffer.as_slice()).unwrap();
        assert_eq!(original, deserialized);
    }

    fn test_header() -> MessageHeader {
        let protocol = ProtocolInfo {
            version_using: 2,
            version_max: 3,
            version_min: 1,
            network: NetworkType::NanoDevNetwork,
        };
        MessageHeader {
            message_type: MessageType::Keepalive,
            protocol,
            extensions: BitArray::from(14),
        }
    }

    #[test]
    fn serialize_header() {
        let protocol_info = ProtocolInfo::default_for(NetworkType::NanoDevNetwork);
        let mut header = MessageHeader::new(MessageType::Publish, protocol_info);
        header.extensions = 0xABCD.into();

        let mut buffer = Vec::new();
        header.serialize(&mut buffer).unwrap();

        assert_eq!(buffer.len(), 8);
        assert_eq!(buffer[0], 0x52);
        assert_eq!(buffer[1], 0x41);
        assert_eq!(buffer[2], protocol_info.version_using);
        assert_eq!(buffer[3], protocol_info.version_max);
        assert_eq!(buffer[4], protocol_info.version_min);
        assert_eq!(buffer[5], 0x03); // publish
        assert_eq!(buffer[6], 0xCD); // extensions
        assert_eq!(buffer[7], 0xAB); // extensions
    }

    #[test]
    fn rai_message_ids_are_stable() {
        assert_eq!(MessageType::RaiVote as u8, MessageType::RAI_VOTE_ID);
        assert_eq!(
            MessageType::RaiPendingReport as u8,
            MessageType::RAI_PENDING_REPORT_ID
        );
    }

    #[test]
    fn rai_messages_require_rai_protocol_version() {
        assert!(
            !MessageType::RaiVote.is_supported_by_protocol(ProtocolInfo::RAI_PROTOCOL_VERSION - 1)
        );
        assert!(MessageType::RaiVote.is_supported_by_protocol(ProtocolInfo::RAI_PROTOCOL_VERSION));
        assert!(
            !MessageType::RaiPendingReport
                .is_supported_by_protocol(ProtocolInfo::RAI_PROTOCOL_VERSION - 1)
        );
        assert!(
            MessageType::RaiPendingReport
                .is_supported_by_protocol(ProtocolInfo::RAI_PROTOCOL_VERSION)
        );
    }
}
