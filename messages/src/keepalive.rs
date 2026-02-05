use rsnano_types::DeserializationError;
use serde_derive::Serialize;
use std::{
    fmt::Display,
    io::Read,
    net::{Ipv6Addr, SocketAddrV6},
};

use super::MessageVariant;

#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Keepalive {
    pub peers: [SocketAddrV6; 8],
}

impl Keepalive {
    pub const SERIALIZED_SIZE: usize = 8 * (16 + 2);

    pub const fn new_test_instance() -> Self {
        Self {
            peers: [
                SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 1, 2, 3, 4), 1111, 0, 0),
                SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 1, 2, 3, 5), 2222, 0, 0),
                SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 1, 2, 3, 6), 3333, 0, 0),
                SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 1, 2, 3, 7), 4444, 0, 0),
                SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 1, 2, 3, 8), 5555, 0, 0),
                SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 1, 2, 3, 9), 6666, 0, 0),
                SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 1, 2, 3, 0x10), 7777, 0, 0),
                SocketAddrV6::new(Ipv6Addr::new(0, 0, 0, 0xffff, 1, 2, 3, 0x11), 8888, 0, 0),
            ],
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        for peer in &self.peers {
            let ip_bytes = peer.ip().octets();
            writer.write_all(&ip_bytes)?;

            let port_bytes = peer.port().to_le_bytes();
            writer.write_all(&port_bytes)?;
        }
        Ok(())
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let mut peers = empty_peers();

        for peer in &mut peers {
            let mut addr_buffer = [0u8; 16];
            let mut port_buffer = [0u8; 2];
            bytes.read_exact(&mut addr_buffer)?;
            bytes.read_exact(&mut port_buffer)?;

            let port = u16::from_le_bytes(port_buffer);
            let ip_addr = Ipv6Addr::from(addr_buffer);

            *peer = SocketAddrV6::new(ip_addr, port, 0, 0);
        }

        Ok(Self { peers })
    }
}

impl Default for Keepalive {
    fn default() -> Self {
        Self {
            peers: empty_peers(),
        }
    }
}

impl MessageVariant for Keepalive {}

impl Display for Keepalive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for peer in &self.peers {
            write!(f, "\n{}", peer)?;
        }
        Ok(())
    }
}

fn empty_peers() -> [SocketAddrV6; 8] {
    [SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0); 8]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn serialize_no_peers() {
        let request = Message::Keepalive(Keepalive::default());
        assert_deserializable(&request);
    }

    #[test]
    fn serialize_peers() {
        let mut keepalive = Keepalive::default();
        keepalive.peers[0] = SocketAddrV6::new(Ipv6Addr::LOCALHOST, 10000, 0, 0);
        let request = Message::Keepalive(keepalive);
        assert_deserializable(&request);
    }
}
