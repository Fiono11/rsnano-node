use super::{Message, MessageHeader};
use rsnano_types::ProtocolInfo;

#[derive(Clone)]
pub struct MessageSerializer {
    protocol: ProtocolInfo,
    buffer: Vec<u8>,
}

impl MessageSerializer {
    const BUFFER_SIZE: usize = MessageHeader::SERIALIZED_SIZE + Message::MAX_MESSAGE_SIZE;
    pub fn new(protocol: ProtocolInfo) -> Self {
        Self {
            protocol,
            buffer: Vec::with_capacity(Self::BUFFER_SIZE),
        }
    }

    pub fn new_with_buffer_size(protocol: ProtocolInfo, buffer_size: usize) -> Self {
        Self {
            protocol,
            buffer: Vec::with_capacity(buffer_size),
        }
    }

    pub fn serialize(&'_ mut self, message: &Message) -> &'_ [u8] {
        self.buffer.resize(MessageHeader::SERIALIZED_SIZE, 0);
        let payload_len;
        {
            message
                .serialize(&mut self.buffer)
                .expect("Writing message body should succeed");
            payload_len = self.buffer.len() - MessageHeader::SERIALIZED_SIZE;
            let payload_len_u16 = u16::try_from(payload_len)
                .expect("message payload exceeds the wire header's u16 length");

            let mut header = MessageHeader::new(message.message_type(), self.protocol);
            header.extensions = message.header_extensions(payload_len_u16);
            header
                .serialize(&mut &mut self.buffer[..MessageHeader::SERIALIZED_SIZE])
                .expect("Writing header should succeed");
        }
        &self.buffer[..MessageHeader::SERIALIZED_SIZE + payload_len]
    }
}

impl Default for MessageSerializer {
    fn default() -> Self {
        Self::new(ProtocolInfo::default())
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use crate::{RaiCloseCutWire, RaiCloseVersionWire, RaiVoteRequest};
    use rsnano_types::{BlockHash, QualifiedRoot, RaiEpoch, RaiSlotId, Root};

    #[test]
    #[should_panic(expected = "message payload exceeds the wire header's u16 length")]
    fn oversized_payload_is_rejected_instead_of_truncating_header_length() {
        let obligations = (0..=crate::MAX_RAI_CLOSE_CUT_CHUNK_ENTRIES)
            .map(|i| RaiSlotId {
                epoch: RaiEpoch::new(1),
                root: QualifiedRoot::new(Root::from(i as u64 + 1), BlockHash::from(2)),
            })
            .collect();
        let message = Message::RaiVoteRequest(RaiVoteRequest {
            sequence: 1,
            epoch: 1,
            hash: BlockHash::from(3),
            root: Root::from(4),
            close_version: Some(RaiCloseVersionWire::Cut(RaiCloseCutWire {
                epoch: 1,
                obligations,
            })),
        });

        MessageSerializer::default().serialize(&message);
    }
}
