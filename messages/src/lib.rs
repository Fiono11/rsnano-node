#[macro_use]
extern crate num_derive;

#[macro_use]
extern crate anyhow;

#[macro_use]
extern crate static_assertions;

#[macro_use]
extern crate strum_macros;

mod message;
mod message_deserializer;
mod network_filter;

pub use message::*;
pub use message_deserializer::*;
pub use network_filter::*;

mod message_serializer;
pub use message_serializer::*;

mod message_header;
pub use message_header::*;

mod handshake;
pub use handshake::{Handshake, HandshakeQuery, HandshakeResponse};

mod keepalive;
pub use keepalive::*;

mod publish;
pub use publish::*;

mod confirm_req;
pub use confirm_req::*;

mod confirm_ack;
pub use confirm_ack::*;

#[cfg(feature = "rai_protocol")]
mod rai;
#[cfg(feature = "rai_protocol")]
mod rai_reconciliation;
#[cfg(feature = "rai_protocol")]
pub use rai_reconciliation::*;

mod frontier_req;
pub use frontier_req::*;

mod bulk_pull;
pub use bulk_pull::*;

mod bulk_pull_account;
pub use bulk_pull_account::*;

mod telemetry_ack;
use rsnano_types::{Blake2Hash, PublicKey};
pub use telemetry_ack::*;

mod asc_pull_req;
pub use asc_pull_req::*;

mod asc_pull_ack;
pub use asc_pull_ack::*;

pub trait MessageVisitor {
    fn received(&mut self, message: &Message);
}

pub trait Aggregatable {
    fn signer(&self) -> PublicKey;

    fn hash(&self) -> Blake2Hash;
}

pub type Cookie = [u8; 32];

pub fn deserialize_message(buffer: &[u8]) -> anyhow::Result<(MessageHeader, Message)> {
    let (mut header_bytes, payload_bytes) = buffer.split_at(MessageHeader::SERIALIZED_SIZE);
    let header = MessageHeader::deserialize(&mut header_bytes)?;
    let message = Message::deserialize(payload_bytes, &header, 0)?;
    Ok((header, message))
}

#[cfg(test)]
pub(crate) fn assert_deserializable(original: &Message) {
    let mut serializer = MessageSerializer::default();
    let mut serialized = serializer.serialize(original);
    let header = MessageHeader::deserialize(&mut serialized).unwrap();
    assert_eq!(
        header.payload_length(),
        serialized.len(),
        "Serialized message has incorrect payload length. Len in header: {}, actual len: {}",
        header.payload_length(),
        serialized.len()
    );
    let message_out = Message::deserialize(serialized, &header, 0).unwrap();
    assert_eq!(message_out, *original);
}
