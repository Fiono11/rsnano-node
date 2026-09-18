use std::fmt::Display;
use std::io::Read;

use serde::{Deserialize, Serialize};

use crate::DeserializationError;

/// RAI consensus epoch: the index of the Kudzu instance a vote belongs to.
/// Not to be confused with the ledger upgrade `Epoch` of a block. Blocks
/// carry no consensus epoch; a slot may be contested in several epochs.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ConsensusEpoch(u64);

impl ConsensusEpoch {
    pub const SERIALIZED_SIZE: usize = size_of::<u64>();
    /// The single implicit epoch of the legacy protocol
    pub const ZERO: ConsensusEpoch = ConsensusEpoch(0);

    pub const fn new(epoch: u64) -> Self {
        Self(epoch)
    }

    pub const fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn next(&self) -> Self {
        Self(self.0 + 1)
    }

    pub fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(&self.0.to_le_bytes())
    }

    pub fn deserialize(bytes: &mut impl Read) -> Result<Self, DeserializationError> {
        let mut buffer = [0; Self::SERIALIZED_SIZE];
        bytes.read_exact(&mut buffer)?;
        Ok(Self(u64::from_le_bytes(buffer)))
    }
}

impl From<u64> for ConsensusEpoch {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl Display for ConsensusEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_round_trip() {
        let epoch = ConsensusEpoch::new(7);
        let mut bytes = Vec::new();
        epoch.serialize(&mut bytes).unwrap();
        assert_eq!(bytes.len(), ConsensusEpoch::SERIALIZED_SIZE);
        assert_eq!(
            ConsensusEpoch::deserialize(&mut bytes.as_slice()).unwrap(),
            epoch
        );
        assert_eq!(ConsensusEpoch::ZERO.next(), ConsensusEpoch::new(1));
    }
}
