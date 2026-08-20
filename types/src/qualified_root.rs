use crate::{BlockHash, DeserializationError, Root};
use primitive_types::U512;
use serde::de::Unexpected;
use std::{
    hash::Hash,
    io::{Read, Write},
};

#[derive(Default, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct QualifiedRoot {
    pub root: Root,
    pub previous: BlockHash,
    #[cfg(feature = "rai_protocol")]
    pub epoch: u64,
}

#[cfg(feature = "rai_protocol")]
#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SlotRoot {
    pub root: Root,
    pub previous: BlockHash,
}

impl QualifiedRoot {
    pub const ZERO: Self = QualifiedRoot::new(Root::ZERO, BlockHash::ZERO);
    pub const SERIALIZED_SIZE: usize = Root::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE;

    pub const fn new(root: Root, previous: BlockHash) -> Self {
        Self {
            root,
            previous,
            #[cfg(feature = "rai_protocol")]
            epoch: 0,
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub const fn slot(&self) -> SlotRoot {
        SlotRoot {
            root: self.root,
            previous: self.previous,
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub const fn with_epoch(mut self, epoch: u64) -> Self {
        self.epoch = epoch;
        self
    }

    pub fn to_bytes(&self) -> [u8; Self::SERIALIZED_SIZE] {
        let mut buffer = [0; Self::SERIALIZED_SIZE];
        self.serialize(&mut buffer.as_mut())
            .expect("Should serialize qualified root");
        buffer
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: Write,
    {
        writer.write_all(self.root.as_bytes())?;
        writer.write_all(self.previous.as_bytes())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<QualifiedRoot, DeserializationError>
    where
        T: Read,
    {
        let root = Root::deserialize(reader)?;
        let previous = BlockHash::deserialize(reader)?;
        Ok(QualifiedRoot::new(root, previous))
    }

    pub fn new_test_instance() -> Self {
        Self::new(Root::from(111), BlockHash::from(222))
    }
    pub fn encode_hex(&self) -> String {
        use std::fmt::Write;
        let mut result = String::with_capacity(128);
        for byte in &self.to_bytes() {
            write!(&mut result, "{:02X}", byte).unwrap();
        }
        result
    }

    pub fn decode_hex(s: impl AsRef<str>) -> Option<Self> {
        let mut bytes = [0u8; 64];
        hex::decode_to_slice(s.as_ref(), &mut bytes).ok()?;

        let mut slice = bytes.as_slice();
        let root = Root::deserialize(&mut slice).ok()?;
        let previous = BlockHash::deserialize(&mut slice).ok()?;
        Some(Self::new(root, previous))
    }
}

impl From<U512> for QualifiedRoot {
    fn from(value: U512) -> Self {
        let bytes = value.to_big_endian();
        let root = Root::from_slice(&bytes[..32]).unwrap();
        let previous = BlockHash::from_slice(&bytes[32..]).unwrap();
        QualifiedRoot::new(root, previous)
    }
}

#[cfg(feature = "rai_protocol")]
impl SlotRoot {
    pub const fn with_epoch(self, epoch: u64) -> QualifiedRoot {
        QualifiedRoot::new(self.root, self.previous).with_epoch(epoch)
    }
}

impl serde::Serialize for QualifiedRoot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!(
            "{}{}",
            self.root.encode_hex(),
            self.previous.encode_hex()
        ))
    }
}

impl<'de> serde::Deserialize<'de> for QualifiedRoot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(QualifiedRootVisitor {})
    }
}

struct QualifiedRootVisitor {}

impl<'de> serde::de::Visitor<'de> for QualifiedRootVisitor {
    type Value = QualifiedRoot;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a qualified root")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        QualifiedRoot::decode_hex(v)
            .ok_or_else(|| serde::de::Error::invalid_value(Unexpected::Str(v), &"a qualified root"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_hex() {
        let hex = "000000000000000000000000000000000000000000000000000000000000007B000000000000000000000000000000000000000000000000000000000000007C";
        let decoded = QualifiedRoot::decode_hex(hex).unwrap();
        assert_eq!(decoded.root, Root::from(123));
        assert_eq!(decoded.previous, BlockHash::from(124));
    }

    #[test]
    fn serialize_json() {
        let root = QualifiedRoot::new(Root::from(0xaabbcc), BlockHash::from(0x112233));
        let json = serde_json::to_string(&root).unwrap();
        assert_eq!(
            json,
            "\"0000000000000000000000000000000000000000000000000000000000AABBCC0000000000000000000000000000000000000000000000000000000000112233\""
        );
    }

    #[test]
    fn deserialize_json() {
        let input = "\"0000000000000000000000000000000000000000000000000000000000AABBCC0000000000000000000000000000000000000000000000000000000000112233\"";
        let expected = QualifiedRoot::new(Root::from(0xaabbcc), BlockHash::from(0x112233));
        let result: QualifiedRoot = serde_json::from_str(input).unwrap();
        assert_eq!(result, expected);
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn epoch_scopes_elections_but_not_slots() {
        let root = QualifiedRoot::new(Root::from(1), BlockHash::from(2));
        assert_eq!(root.epoch, 0);
        let first = root.clone().with_epoch(1);
        let second = root.with_epoch(2);
        assert_ne!(first, second);
        assert_eq!(first.slot(), second.slot());
    }
}
