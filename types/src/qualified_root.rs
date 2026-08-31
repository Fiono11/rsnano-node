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

impl QualifiedRoot {
    pub const ZERO: Self = QualifiedRoot::new(Root::ZERO, BlockHash::ZERO);
    pub const SERIALIZED_SIZE: usize = Root::SERIALIZED_SIZE
        + BlockHash::SERIALIZED_SIZE
        + if cfg!(feature = "rai_protocol") { 8 } else { 0 };

    pub const fn new(root: Root, previous: BlockHash) -> Self {
        Self {
            root,
            previous,
            #[cfg(feature = "rai_protocol")]
            epoch: 0,
        }
    }

    #[cfg(feature = "rai_protocol")]
    pub const fn with_epoch(root: Root, previous: BlockHash, epoch: u64) -> Self {
        Self { root, previous, epoch }
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
        writer.write_all(self.previous.as_bytes())?;
        #[cfg(feature = "rai_protocol")]
        writer.write_all(&self.epoch.to_le_bytes())?;
        Ok(())
    }

    pub fn deserialize<T>(reader: &mut T) -> Result<QualifiedRoot, DeserializationError>
    where
        T: Read,
    {
        let root = Root::deserialize(reader)?;
        let previous = BlockHash::deserialize(reader)?;
        #[cfg(feature = "rai_protocol")]
        let epoch = {
            let mut bytes = [0; 8];
            reader.read_exact(&mut bytes)?;
            u64::from_le_bytes(bytes)
        };
        Ok(QualifiedRoot {
            root,
            previous,
            #[cfg(feature = "rai_protocol")]
            epoch,
        })
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
        let bytes = hex::decode(s.as_ref()).ok()?;
        #[cfg(not(feature = "rai_protocol"))]
        if bytes.len() != Self::SERIALIZED_SIZE {
            return None;
        }
        #[cfg(feature = "rai_protocol")]
        if bytes.len() != 64 && bytes.len() != Self::SERIALIZED_SIZE {
            return None;
        }

        let mut slice = bytes.as_slice();
        let root = Root::deserialize(&mut slice).ok()?;
        let previous = BlockHash::deserialize(&mut slice).ok()?;
        #[cfg(feature = "rai_protocol")]
        let epoch = if slice.is_empty() {
            0
        } else {
            let mut bytes = [0; 8];
            slice.read_exact(&mut bytes).ok()?;
            u64::from_le_bytes(bytes)
        };
        Some(Self {
            root,
            previous,
            #[cfg(feature = "rai_protocol")]
            epoch,
        })
    }
}

impl From<U512> for QualifiedRoot {
    fn from(value: U512) -> Self {
        let bytes = value.to_big_endian();
        let root = Root::from_slice(&bytes[..32]).unwrap();
        let previous = BlockHash::from_slice(&bytes[32..]).unwrap();
        QualifiedRoot {
            root,
            previous,
            #[cfg(feature = "rai_protocol")]
            epoch: 0,
        }
    }
}

impl serde::Serialize for QualifiedRoot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.encode_hex())
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
        #[cfg(feature = "rai_protocol")]
        assert_eq!(decoded.epoch, 0);
    }

    #[test]
    fn serialize_json() {
        let root = QualifiedRoot::new(Root::from(0xaabbcc), BlockHash::from(0x112233));
        let json = serde_json::to_string(&root).unwrap();
        let suffix = if cfg!(feature = "rai_protocol") {
            "0000000000000000"
        } else {
            ""
        };
        assert_eq!(
            json,
            format!("\"0000000000000000000000000000000000000000000000000000000000AABBCC0000000000000000000000000000000000000000000000000000000000112233{suffix}\"")
        );
    }

    #[test]
    fn deserialize_json() {
        let input = "\"0000000000000000000000000000000000000000000000000000000000AABBCC0000000000000000000000000000000000000000000000000000000000112233\"";
        let expected = QualifiedRoot::new(Root::from(0xaabbcc), BlockHash::from(0x112233));
        let result: QualifiedRoot = serde_json::from_str(input).unwrap();
        assert_eq!(result, expected);
    }
}
