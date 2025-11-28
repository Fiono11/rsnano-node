use crate::{Account, PrivateKey, PublicKey, currency_constants::ACCOUNT_PREFIX};
use serde::de::{Unexpected, Visitor};
use std::{fmt::Display, io::Read, str::FromStr};

#[derive(PartialEq, Eq, Clone, Copy, Hash, Default, PartialOrd, Ord)]
pub struct NodeId([u8; 32]);

impl NodeId {
    pub const ZERO: Self = NodeId::from_bytes([0; 32]);

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_key(&self) -> PublicKey {
        PublicKey::from_bytes(self.0)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        writer.write_all(self.as_bytes())
    }

    pub fn deserialize<T>(reader: &mut T) -> std::io::Result<Self>
    where
        T: Read,
    {
        let mut result = Self::ZERO;
        reader.read_exact(&mut result.0)?;
        Ok(result)
    }
}

impl From<i32> for NodeId {
    fn from(value: i32) -> Self {
        let mut bytes = [0; 32];
        bytes[28..].copy_from_slice(&value.to_be_bytes());
        Self::from_bytes(bytes)
    }
}

impl From<u64> for NodeId {
    fn from(value: u64) -> Self {
        let mut bytes = [0; 32];
        bytes[24..].copy_from_slice(&value.to_be_bytes());
        Self::from_bytes(bytes)
    }
}

impl From<u128> for NodeId {
    fn from(value: u128) -> Self {
        let mut bytes = [0; 32];
        bytes[16..].copy_from_slice(&value.to_be_bytes());
        Self::from_bytes(bytes)
    }
}

impl Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut result = Account::from_bytes(self.0).encode_account();
        result.replace_range(0..ACCOUNT_PREFIX.len(), "node");
        write!(f, "{}", result)
    }
}

impl std::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self, f)
    }
}

impl FromStr for NodeId {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut node_id = s.to_string();
        if node_id.starts_with("node_") {
            node_id.replace_range(0..4, ACCOUNT_PREFIX);
            let account = Account::parse(node_id).ok_or(())?;
            Ok(Self::from_bytes(*account.as_bytes()))
        } else {
            Err(())
        }
    }
}

impl serde::Serialize for NodeId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for NodeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = deserializer.deserialize_str(NodeIdVisitor {})?;
        Ok(value)
    }
}

struct NodeIdVisitor {}

impl<'de> Visitor<'de> for NodeIdVisitor {
    type Value = NodeId;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a node ID in the form \"node_...\"")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        v.parse::<NodeId>().map_err(|_| {
            serde::de::Error::invalid_value(
                Unexpected::Str(v),
                &"a node ID in the form \"node_...\"",
            )
        })
    }
}

impl From<NodeId> for PublicKey {
    fn from(value: NodeId) -> Self {
        value.as_key()
    }
}

impl From<PublicKey> for NodeId {
    fn from(value: PublicKey) -> Self {
        Self::from_bytes(*value.as_bytes())
    }
}

impl From<&PrivateKey> for NodeId {
    fn from(value: &PrivateKey) -> Self {
        value.public_key().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_string() {
        assert_eq!(
            NodeId::from(123).to_string(),
            "node_111111111111111111111111111111111111111111111111115uwdgas549"
        );
    }

    #[test]
    fn parse() {
        assert_eq!(
            "node_111111111111111111111111111111111111111111111111115uwdgas549"
                .parse::<NodeId>()
                .unwrap(),
            NodeId::from(123),
        );
    }

    #[test]
    fn parse_fails() {
        assert!("invalid".parse::<NodeId>().is_err());
    }

    #[test]
    fn json_serialize() {
        let json = serde_json::to_string(&NodeId::from(123)).unwrap();
        assert_eq!(
            json,
            "\"node_111111111111111111111111111111111111111111111111115uwdgas549\""
        )
    }

    #[test]
    fn json_deserialize() {
        let json = "\"node_111111111111111111111111111111111111111111111111115uwdgas549\"";
        assert_eq!(
            serde_json::from_str::<NodeId>(&json).unwrap(),
            NodeId::from(123)
        );
    }

    #[test]
    fn json_deserialize_error() {
        let json = "\"invalid\"";
        let error = serde_json::from_str::<NodeId>(&json).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("a node ID in the form \"node_...\"")
        );
    }
}
