use std::io::{Read, Write};

use bitvec::prelude::BitArray;
use rsnano_types::{
    Account, BlockHash, ConfirmationHeightInfo, DeserializationError, RaiEpoch, read_u64_be,
};

use crate::MessageVariant;

/// Canonical close-record frontier preimage used for hash reconciliation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiFrontierMessage {
    pub epoch: RaiEpoch,
    pub previous: BlockHash,
    pub frontiers: Vec<(Account, ConfirmationHeightInfo)>,
}

impl RaiFrontierMessage {
    pub fn serialize<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&self.epoch.number().to_be_bytes())?;
        writer.write_all(self.previous.as_bytes())?;
        writer.write_all(&(self.frontiers.len() as u32).to_be_bytes())?;
        for (account, info) in &self.frontiers {
            writer.write_all(account.as_bytes())?;
            writer.write_all(&info.height.to_be_bytes())?;
            writer.write_all(info.frontier.as_bytes())?;
        }
        Ok(())
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let epoch = RaiEpoch::new(read_u64_be(&mut bytes)?);
        let previous = BlockHash::deserialize(&mut bytes)?;
        let mut count = [0; 4];
        bytes.read_exact(&mut count)?;
        let count = u32::from_be_bytes(count) as usize;
        if bytes.len() != count * 72 {
            return Err(DeserializationError::InvalidData);
        }
        let mut frontiers = Vec::with_capacity(count);
        for _ in 0..count {
            let account = Account::deserialize(&mut bytes)?;
            let height = read_u64_be(&mut bytes)?;
            let frontier = BlockHash::deserialize(&mut bytes)?;
            frontiers.push((account, ConfirmationHeightInfo::new(height, frontier)));
        }
        frontiers.sort_by_key(|(account, _)| *account);
        if frontiers.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            epoch,
            previous,
            frontiers,
        })
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }
}

impl MessageVariant for RaiFrontierMessage {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn roundtrip() {
        assert_deserializable(&Message::RaiFrontier(RaiFrontierMessage {
            epoch: 2.into(),
            previous: 3.into(),
            frontiers: vec![(4.into(), ConfirmationHeightInfo::new(5, 6.into()))],
        }));
    }
}
