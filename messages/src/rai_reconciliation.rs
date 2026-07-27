use bitvec::prelude::BitArray;
use rsnano_types::{
    Account, BlockHash, DeserializationError, RaiEpoch, RaiSlot, read_u8, read_u64_be,
};
use std::io::Write;

use crate::MessageVariant;

const REQUEST: u16 = 0;
const MISS: u16 = 1;
const CUT_DELTA: u16 = 2;
const FRONTIER_DELTA: u16 = 3;
const KIND_MASK: u16 = 0x3;
const MAX_ITEMS: usize = (u16::MAX >> 2) as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaiReconKind {
    Cut,
    Frontiers,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiReconRequest {
    pub kind: RaiReconKind,
    pub epoch: RaiEpoch,
    pub base_hash: BlockHash,
    pub target_hash: BlockHash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCutDeltaItem {
    pub added: bool,
    pub slot: RaiSlot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiFrontierDeltaItem {
    pub account: Account,
    pub old: Option<BlockHash>,
    pub new: Option<BlockHash>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiReconciliation {
    Request(RaiReconRequest),
    Miss(RaiReconRequest),
    CutDelta {
        epoch: RaiEpoch,
        base_hash: BlockHash,
        target_hash: BlockHash,
        items: Vec<RaiCutDeltaItem>,
    },
    FrontierDelta {
        epoch: RaiEpoch,
        base_hash: BlockHash,
        target_hash: BlockHash,
        items: Vec<RaiFrontierDeltaItem>,
    },
}

impl MessageVariant for RaiReconciliation {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        let (kind, count) = match self {
            Self::Request(_) => (REQUEST, 0),
            Self::Miss(_) => (MISS, 0),
            Self::CutDelta { items, .. } => (CUT_DELTA, items.len()),
            Self::FrontierDelta { items, .. } => (FRONTIER_DELTA, items.len()),
        };
        assert!(count <= MAX_ITEMS);
        BitArray::new(kind | ((count as u16) << 2))
    }
}

impl RaiReconciliation {
    const PREFIX_SIZE: usize = 8 + 32 + 32;
    const REQUEST_SIZE: usize = 1 + Self::PREFIX_SIZE;
    const CUT_ITEM_SIZE: usize = 1 + RaiSlot::SERIALIZED_SIZE;
    const FRONTIER_ITEM_SIZE: usize = Account::SERIALIZED_SIZE + 1 + 32 + 32;

    pub fn serialized_size(extensions: BitArray<u16>) -> usize {
        let count = (extensions.data >> 2) as usize;
        match extensions.data & KIND_MASK {
            REQUEST | MISS => Self::REQUEST_SIZE,
            CUT_DELTA => Self::PREFIX_SIZE + count * Self::CUT_ITEM_SIZE,
            FRONTIER_DELTA => Self::PREFIX_SIZE + count * Self::FRONTIER_ITEM_SIZE,
            _ => unreachable!(),
        }
    }

    fn write_prefix<W: Write>(
        writer: &mut W,
        epoch: RaiEpoch,
        base: &BlockHash,
        target: &BlockHash,
    ) -> std::io::Result<()> {
        writer.write_all(&epoch.to_be_bytes())?;
        base.serialize(writer)?;
        target.serialize(writer)
    }

    pub fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        match self {
            Self::Request(r) | Self::Miss(r) => {
                writer.write_all(&[match r.kind {
                    RaiReconKind::Cut => 0,
                    RaiReconKind::Frontiers => 1,
                }])?;
                Self::write_prefix(writer, r.epoch, &r.base_hash, &r.target_hash)
            }
            Self::CutDelta {
                epoch,
                base_hash,
                target_hash,
                items,
            } => {
                Self::write_prefix(writer, *epoch, base_hash, target_hash)?;
                for item in items {
                    writer.write_all(&[u8::from(item.added)])?;
                    item.slot.serialize(writer)?;
                }
                Ok(())
            }
            Self::FrontierDelta {
                epoch,
                base_hash,
                target_hash,
                items,
            } => {
                Self::write_prefix(writer, *epoch, base_hash, target_hash)?;
                for item in items {
                    item.account.serialize(writer)?;
                    writer.write_all(&[
                        u8::from(item.old.is_some()) | (u8::from(item.new.is_some()) << 1)
                    ])?;
                    item.old.unwrap_or_default().serialize(writer)?;
                    item.new.unwrap_or_default().serialize(writer)?;
                }
                Ok(())
            }
        }
    }

    pub fn deserialize(
        mut bytes: &[u8],
        extensions: BitArray<u16>,
    ) -> Result<Self, DeserializationError> {
        let variant = extensions.data & KIND_MASK;
        let count = (extensions.data >> 2) as usize;
        let request_kind = if matches!(variant, REQUEST | MISS) {
            Some(match read_u8(&mut bytes)? {
                0 => RaiReconKind::Cut,
                1 => RaiReconKind::Frontiers,
                _ => return Err(DeserializationError::InvalidData),
            })
        } else {
            None
        };
        let epoch = read_u64_be(&mut bytes)?;
        let base_hash = BlockHash::deserialize(&mut bytes)?;
        let target_hash = BlockHash::deserialize(&mut bytes)?;
        let result = match variant {
            REQUEST | MISS => {
                if count != 0 {
                    return Err(DeserializationError::InvalidData);
                }
                let request = RaiReconRequest {
                    kind: request_kind.unwrap(),
                    epoch,
                    base_hash,
                    target_hash,
                };
                if variant == REQUEST {
                    Self::Request(request)
                } else {
                    Self::Miss(request)
                }
            }
            CUT_DELTA => {
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    let added = match read_u8(&mut bytes)? {
                        0 => false,
                        1 => true,
                        _ => return Err(DeserializationError::InvalidData),
                    };
                    items.push(RaiCutDeltaItem {
                        added,
                        slot: RaiSlot::deserialize(&mut bytes)?,
                    });
                }
                Self::CutDelta {
                    epoch,
                    base_hash,
                    target_hash,
                    items,
                }
            }
            FRONTIER_DELTA => {
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    let account = Account::deserialize(&mut bytes)?;
                    let flags = read_u8(&mut bytes)?;
                    if flags > 3 {
                        return Err(DeserializationError::InvalidData);
                    }
                    let old_hash = BlockHash::deserialize(&mut bytes)?;
                    let new_hash = BlockHash::deserialize(&mut bytes)?;
                    let old = (flags & 1 != 0).then_some(old_hash);
                    let new = (flags & 2 != 0).then_some(new_hash);
                    if old.is_some_and(|h| h.is_zero()) || new.is_some_and(|h| h.is_zero()) {
                        return Err(DeserializationError::InvalidData);
                    }
                    items.push(RaiFrontierDeltaItem { account, old, new });
                }
                Self::FrontierDelta {
                    epoch,
                    base_hash,
                    target_hash,
                    items,
                }
            }
            _ => return Err(DeserializationError::InvalidData),
        };
        if !bytes.is_empty() {
            return Err(DeserializationError::TooMuchData);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    fn request() -> RaiReconRequest {
        RaiReconRequest {
            kind: RaiReconKind::Cut,
            epoch: 7,
            base_hash: BlockHash::from(8),
            target_hash: BlockHash::from(9),
        }
    }

    #[test]
    fn request_round_trip() {
        assert_deserializable(&Message::RaiReconciliation(RaiReconciliation::Request(
            request(),
        )));
    }

    #[test]
    fn miss_round_trip() {
        assert_deserializable(&Message::RaiReconciliation(RaiReconciliation::Miss(
            request(),
        )));
    }

    #[test]
    fn cut_delta_round_trip() {
        assert_deserializable(&Message::RaiReconciliation(RaiReconciliation::CutDelta {
            epoch: 7,
            base_hash: BlockHash::from(8),
            target_hash: BlockHash::from(9),
            items: vec![RaiCutDeltaItem {
                added: true,
                slot: RaiSlot::new(Account::from(1), 2),
            }],
        }));
    }

    #[test]
    fn frontier_delta_round_trip() {
        assert_deserializable(&Message::RaiReconciliation(
            RaiReconciliation::FrontierDelta {
                epoch: 7,
                base_hash: BlockHash::from(8),
                target_hash: BlockHash::from(9),
                items: vec![RaiFrontierDeltaItem {
                    account: Account::from(1),
                    old: Some(BlockHash::from(2)),
                    new: Some(BlockHash::from(3)),
                }],
            },
        ));
    }
}
