use std::{
    fmt::Display,
    io::Read,
    mem::size_of,
    time::{Duration, SystemTime},
};

use bitvec::prelude::BitArray;
use serde_derive::Serialize;

use rsnano_types::{Account, BlockHash, NodeId, PrivateKey, Signature, to_hex_string};
use rsnano_types::{DeserializationError, read_u8, read_u32_be, read_u64_be};

use super::MessageVariant;

#[repr(u8)]
#[derive(FromPrimitive, Copy, Clone, PartialEq, Eq)]
pub enum TelemetryMaker {
    NfNode = 0,
    NfPrunedNode = 1,
    NanoNodeLight = 2,
    RsNano = 3,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TelemetryData {
    pub signature: Signature,
    pub node_id: NodeId,
    pub block_count: u64,
    pub cemented_count: u64,
    pub unchecked_count: u64,
    pub account_count: u64,
    pub bandwidth_cap: u64,
    pub uptime: u64,
    pub peer_count: u32,
    pub protocol_version: u8,
    pub genesis_block: BlockHash,
    pub major_version: u8,
    pub minor_version: u8,
    pub patch_version: u8,
    pub pre_release_version: u8,
    pub maker: u8, // Where this telemetry information originated
    pub timestamp: SystemTime,
    pub active_difficulty: u64,
    pub unknown_data: Vec<u8>,
}

impl TelemetryData {
    pub const SIZE_MASK: u16 = 0x3ff;

    /// Size does not include unknown_data
    pub const SERIALIZED_SIZE_OF_KNOWN_DATA: usize =
        Signature::SERIALIZED_SIZE
        + Account::SERIALIZED_SIZE // node id
        + size_of::<u64>() //block_count
          + size_of::<u64>()// cemented_count 
          + size_of::<u64>() // unchecked_count 
          + size_of::<u64>() // account_count 
          + size_of::<u64>() // bandwidth_cap 
          + size_of::<u32>() // peer_count
          + size_of::<u8>() // protocol_version
          + size_of::<u64>() // uptime 
          + BlockHash::SERIALIZED_SIZE
          + size_of::<u8>() // major_version 
          + size_of::<u8>() // minor_version 
          + size_of::<u8>() // patch_version 
          + size_of::<u8>() // pre_release_version 
          + size_of::<u8>() // maker 
          + size_of::<u64>() // timestamp 
          + size_of::<u64>() //active_difficulty)
    ;

    pub fn new() -> Self {
        Self {
            signature: Signature::new(),
            node_id: NodeId::ZERO,
            block_count: 0,
            cemented_count: 0,
            unchecked_count: 0,
            account_count: 0,
            bandwidth_cap: 0,
            uptime: 0,
            peer_count: 0,
            protocol_version: 0,
            genesis_block: BlockHash::ZERO,
            major_version: 0,
            minor_version: 0,
            patch_version: 0,
            pre_release_version: 0,
            maker: TelemetryMaker::RsNano as u8,
            timestamp: SystemTime::UNIX_EPOCH,
            active_difficulty: 0,
            unknown_data: Vec::new(),
        }
    }

    pub fn new_test_instance() -> Self {
        let mut data = TelemetryData::new();
        data.node_id = NodeId::from(42);
        data.major_version = 20;
        data.minor_version = 1;
        data.patch_version = 5;
        data.pre_release_version = 2;
        data.maker = TelemetryMaker::RsNano as u8;
        data.timestamp = SystemTime::UNIX_EPOCH + Duration::from_millis(100);
        data
    }

    pub fn serialize_without_signature<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        // All values should be serialized in big endian
        self.node_id.serialize(writer)?;
        writer.write_all(&self.block_count.to_be_bytes())?;
        writer.write_all(&self.cemented_count.to_be_bytes())?;
        writer.write_all(&self.unchecked_count.to_be_bytes())?;
        writer.write_all(&self.account_count.to_be_bytes())?;
        writer.write_all(&self.bandwidth_cap.to_be_bytes())?;
        writer.write_all(&self.peer_count.to_be_bytes())?;
        writer.write_all(&[self.protocol_version])?;
        writer.write_all(&self.uptime.to_be_bytes())?;
        self.genesis_block.serialize(writer)?;
        writer.write_all(&[
            self.major_version,
            self.minor_version,
            self.patch_version,
            self.pre_release_version,
            self.maker,
        ])?;
        writer.write_all(
            &(self
                .timestamp
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64)
                .to_be_bytes(),
        )?;
        writer.write_all(&self.active_difficulty.to_be_bytes())?;
        writer.write_all(&self.unknown_data)
    }

    pub fn deserialize<T>(reader: &mut T, payload_len: usize) -> std::io::Result<Self>
    where
        T: Read,
    {
        let signature = Signature::deserialize(reader)?;
        let node_id = NodeId::deserialize(reader)?;

        let block_count = read_u64_be(reader)?;
        let cemented_count = read_u64_be(reader)?;
        let unchecked_count = read_u64_be(reader)?;
        let account_count = read_u64_be(reader)?;
        let bandwidth_cap = read_u64_be(reader)?;
        let peer_count = read_u32_be(reader)?;
        let protocol_version = read_u8(reader)?;
        let uptime = read_u64_be(reader)?;
        let genesis_block = BlockHash::deserialize(reader)?;
        let major_version = read_u8(reader)?;
        let minor_version = read_u8(reader)?;
        let patch_version = read_u8(reader)?;
        let pre_release_version = read_u8(reader)?;
        let maker = read_u8(reader)?;
        let timestamp_ms = read_u64_be(reader)?;
        let active_difficulty = read_u64_be(reader)?;
        let mut unknown_data = Vec::new();
        if payload_len > TelemetryData::SERIALIZED_SIZE_OF_KNOWN_DATA {
            let unknown_len = payload_len - TelemetryData::SERIALIZED_SIZE_OF_KNOWN_DATA;
            unknown_data.resize(unknown_len, 0);
            reader.read_exact(&mut unknown_data)?;
        }

        let data = TelemetryData {
            signature,
            node_id,
            block_count,
            cemented_count,
            unchecked_count,
            account_count,
            bandwidth_cap,
            peer_count,
            protocol_version,
            uptime,
            genesis_block,
            major_version,
            minor_version,
            patch_version,
            pre_release_version,
            maker,
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_millis(timestamp_ms),
            active_difficulty,
            unknown_data,
        };

        Ok(data)
    }

    pub fn sign(&mut self, key: &PrivateKey) -> anyhow::Result<()> {
        debug_assert!(key.public_key() == self.node_id.into());
        let mut buffer = Vec::new();
        self.serialize_without_signature(&mut buffer)
            .expect("Should succeed serializing telemetry data");
        self.signature = key.sign(&buffer);
        Ok(())
    }

    pub fn validate_signature(&self) -> bool {
        let mut buffer = Vec::new();
        self.serialize_without_signature(&mut buffer)
            .expect("Should succeed serializing telemetry data");
        self.node_id
            .as_key()
            .verify(&buffer, &self.signature)
            .is_ok()
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        let ignore_identification_metrics = true;
        let json_dto = TelemetryDataJsonDto {
            block_count: self.block_count.to_string(),
            cemented_count: self.cemented_count.to_string(),
            unchecked_count: self.unchecked_count.to_string(),
            account_count: self.account_count.to_string(),
            bandwidth_cap: self.bandwidth_cap.to_string(),
            peer_count: self.peer_count.to_string(),
            protocol_version: self.protocol_version.to_string(),
            uptime: self.uptime.to_string(),
            genesis_block: self.genesis_block.to_string(),
            major_version: self.major_version.to_string(),
            minor_version: self.minor_version.to_string(),
            patch_version: self.patch_version.to_string(),
            pre_release_version: self.pre_release_version.to_string(),
            maker: self.maker.to_string(),
            timestamp: self
                .timestamp
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .to_string(),
            active_difficulty: to_hex_string(self.active_difficulty),
            node_id: if !ignore_identification_metrics {
                Some(self.node_id.to_string())
            } else {
                None
            },
            signature: if !ignore_identification_metrics {
                Some(self.signature.encode_hex())
            } else {
                None
            },
        };

        serde_json::to_string_pretty(&json_dto)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryAck(pub Option<TelemetryData>);

impl TelemetryAck {
    pub fn new_test_instance() -> Self {
        Self(Some(TelemetryData::new_test_instance()))
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        (extensions.data & TelemetryData::SIZE_MASK) as usize
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        if let Some(data) = &self.0 {
            data.signature.serialize(writer)?;
            data.serialize_without_signature(writer)?;
        }
        Ok(())
    }

    pub fn deserialize(
        mut bytes: &[u8],
        extensions: BitArray<u16>,
    ) -> Result<Self, DeserializationError> {
        let payload_length = Self::serialized_size(extensions);
        if payload_length == 0 {
            return Ok(Self(None));
        }

        let result = TelemetryData::deserialize(&mut bytes, payload_length)?;
        Ok(Self(Some(result)))
    }
}

impl Display for TelemetryAck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "telemetry_ack")
    }
}

impl MessageVariant for TelemetryAck {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        match &self.0 {
            Some(data) => BitArray::new(
                TelemetryData::SERIALIZED_SIZE_OF_KNOWN_DATA as u16
                    + data.unknown_data.len() as u16,
            ),
            None => Default::default(),
        }
    }
}

#[derive(Serialize)]
struct TelemetryDataJsonDto {
    pub block_count: String,
    pub cemented_count: String,
    pub unchecked_count: String,
    pub account_count: String,
    pub bandwidth_cap: String,
    pub peer_count: String,
    pub protocol_version: String,
    pub uptime: String,
    pub genesis_block: String,
    pub major_version: String,
    pub minor_version: String,
    pub patch_version: String,
    pub pre_release_version: String,
    pub maker: String,
    pub timestamp: String,
    pub active_difficulty: String,
    // Keep these last for UI purposes:
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl Default for TelemetryData {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for TelemetryData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f)?;
        write!(f, "{}", self.to_json().map_err(|_| std::fmt::Error)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn serialized_size() {
        assert_eq!(TelemetryData::SERIALIZED_SIZE_OF_KNOWN_DATA, 202);
    }

    #[test]
    fn sign_telemetry_data() {
        let keys = PrivateKey::new();
        let mut data = test_data(&keys);
        data.sign(&keys).unwrap();
        assert_eq!(data.validate_signature(), true);

        let old_signature = data.signature.clone();
        // Check that the signature is different if changing a piece of data
        data.maker = 2;
        data.sign(&keys).unwrap();
        assert_ne!(old_signature, data.signature);
    }

    //original test: telemetry.unknown_data
    #[test]
    fn sign_with_unknown_data() {
        let keys = PrivateKey::new();
        let mut data = test_data(&keys);
        data.unknown_data = vec![1];
        data.sign(&keys).unwrap();
        assert_eq!(data.validate_signature(), true);
    }

    #[test]
    fn max_possible_size() {
        let keys = PrivateKey::new();
        let mut data = test_data(&keys);
        data.unknown_data = vec![
            1;
            TelemetryData::SIZE_MASK as usize
                - TelemetryData::SERIALIZED_SIZE_OF_KNOWN_DATA
        ];

        assert_deserializable(&Message::TelemetryAck(TelemetryAck(Some(data))));
    }

    fn test_data(keys: &PrivateKey) -> TelemetryData {
        let mut data = TelemetryData::new();
        data.node_id = keys.public_key().into();
        data.major_version = 20;
        data.minor_version = 1;
        data.patch_version = 5;
        data.pre_release_version = 2;
        data.maker = 1;
        data.timestamp = SystemTime::UNIX_EPOCH + Duration::from_millis(100);
        data
    }
}
