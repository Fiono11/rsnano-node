use bitvec::prelude::BitArray;

use rsnano_types::{BlockHash, ConsensusEpoch, DeserializationError};

use crate::MessageVariant;

/// RAI: which of a report's two inventories a symbol stream encodes
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReportSet {
    /// The certified state, signed as the report's certified root
    Certified,
    /// The residual vote records, signed as the report's residual root
    Residual,
}

impl ReportSet {
    fn as_byte(self) -> u8 {
        match self {
            ReportSet::Certified => 0,
            ReportSet::Residual => 1,
        }
    }

    fn from_byte(byte: u8) -> Result<Self, DeserializationError> {
        match byte {
            0 => Ok(ReportSet::Certified),
            1 => Ok(ReportSet::Residual),
            _ => Err(DeserializationError::InvalidData),
        }
    }
}

/// RAI, report reconciliation: a request for the coded symbols
/// `from .. from + count` of the inventory with the given root. Any node
/// holding an inventory with that root answers; the root alone names the
/// set, so the reporter's identity is not needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportSymbolsReq {
    pub epoch: ConsensusEpoch,
    pub set: ReportSet,
    pub target: BlockHash,
    pub from: u32,
    pub count: u16,
}

impl ReportSymbolsReq {
    pub const SERIALIZED_SIZE: usize =
        ConsensusEpoch::SERIALIZED_SIZE + 1 + BlockHash::SERIALIZED_SIZE + 4 + 2;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(3),
            set: ReportSet::Residual,
            target: BlockHash::from(7),
            from: 64,
            count: 128,
        }
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        writer.write_all(&[self.set.as_byte()])?;
        self.target.serialize(writer)?;
        writer.write_all(&self.from.to_le_bytes())?;
        writer.write_all(&self.count.to_le_bytes())
    }

    pub const fn serialized_size(_extensions: BitArray<u16>) -> usize {
        Self::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let set = ReportSet::from_byte(take::<1>(bytes)?[0])?;
        let target = BlockHash::deserialize(bytes)?;
        let from = u32::from_le_bytes(take::<4>(bytes)?);
        let count = u16::from_le_bytes(take::<2>(bytes)?);
        Ok(Self {
            epoch,
            set,
            target,
            from,
            count,
        })
    }
}

impl MessageVariant for ReportSymbolsReq {}

/// RAI, report reconciliation: coded symbols `from ..` of the inventory
/// with the given root, as raw bytes of `SYMBOL_SIZE` each
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportSymbolsReply {
    pub epoch: ConsensusEpoch,
    pub set: ReportSet,
    pub target: BlockHash,
    pub from: u32,
    pub symbols: Vec<u8>,
}

impl ReportSymbolsReply {
    /// One coded symbol: count (8), item sum (105), check (8)
    pub const SYMBOL_SIZE: usize = 121;
    /// Symbols per reply: 500 × 121 bytes stays under the 65,535-byte frame
    pub const MAX_SYMBOLS: usize = 500;
    const HEADER_SIZE: usize = ConsensusEpoch::SERIALIZED_SIZE + 1 + BlockHash::SERIALIZED_SIZE + 4;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(3),
            set: ReportSet::Certified,
            target: BlockHash::from(7),
            from: 64,
            symbols: (0..2 * Self::SYMBOL_SIZE).map(|i| i as u8).collect(),
        }
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len() / Self::SYMBOL_SIZE
    }

    pub fn symbols(&self) -> impl Iterator<Item = &[u8]> {
        self.symbols.chunks_exact(Self::SYMBOL_SIZE)
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.epoch.serialize(writer)?;
        writer.write_all(&[self.set.as_byte()])?;
        self.target.serialize(writer)?;
        writer.write_all(&self.from.to_le_bytes())?;
        writer.write_all(&self.symbols)
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let set = ReportSet::from_byte(take::<1>(bytes)?[0])?;
        let target = BlockHash::deserialize(bytes)?;
        let from = u32::from_le_bytes(take::<4>(bytes)?);
        if bytes.len() % Self::SYMBOL_SIZE != 0
            || bytes.len() / Self::SYMBOL_SIZE > Self::MAX_SYMBOLS
        {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            epoch,
            set,
            target,
            from,
            symbols: bytes.to_vec(),
        })
    }
}

impl MessageVariant for ReportSymbolsReply {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

fn take<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N], DeserializationError> {
    if bytes.len() < N {
        return Err(DeserializationError::InvalidData);
    }
    let (head, tail) = bytes.split_at(N);
    *bytes = tail;
    let mut result = [0; N];
    result.copy_from_slice(head);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn serialize_a_symbols_request() {
        assert_deserializable(&Message::ReportSymbolsReq(
            ReportSymbolsReq::new_test_instance(),
        ));
    }

    #[test]
    fn serialize_a_symbols_reply() {
        assert_deserializable(&Message::ReportSymbolsReply(
            ReportSymbolsReply::new_test_instance(),
        ));
    }

    #[test]
    fn a_full_reply_fits_the_frame() {
        let mut reply = ReportSymbolsReply::new_test_instance();
        reply.symbols =
            vec![0xAB; ReportSymbolsReply::MAX_SYMBOLS * ReportSymbolsReply::SYMBOL_SIZE];
        let mut bytes = Vec::new();
        reply.serialize(&mut bytes).unwrap();
        assert!(bytes.len() <= u16::MAX as usize);
        assert_eq!(reply.symbol_count(), ReportSymbolsReply::MAX_SYMBOLS);
        assert_deserializable(&Message::ReportSymbolsReply(reply));
    }

    #[test]
    fn a_partial_symbol_is_refused() {
        let mut bytes = Vec::new();
        let mut reply = ReportSymbolsReply::new_test_instance();
        reply.symbols.push(1);
        reply.serialize(&mut bytes).unwrap();
        assert!(ReportSymbolsReply::deserialize(&bytes).is_err());
    }
}
