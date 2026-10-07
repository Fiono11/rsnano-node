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

    /// RAI: the Kudzu instance of round `round` of the close election of
    /// `epoch`. A close round is an instance like any other; it is told apart
    /// from the epochs of the block elections by the top bit.
    pub const fn close_round(epoch: ConsensusEpoch, round: u32) -> Self {
        assert!(epoch.0 < Self::CLOSE_FLAG >> Self::ROUND_BITS);
        assert!(round < 1 << Self::ROUND_BITS);
        Self(Self::CLOSE_FLAG | (epoch.0 << Self::ROUND_BITS) | round as u64)
    }

    /// RAI: whether this is a close round rather than an epoch
    pub const fn is_close_round(&self) -> bool {
        self.0 & Self::CLOSE_FLAG != 0
    }

    /// RAI: the epoch and round of a close round
    pub const fn as_close_round(&self) -> Option<(ConsensusEpoch, u32)> {
        if !self.is_close_round() {
            return None;
        }
        let bits = self.0 & !Self::CLOSE_FLAG;
        Some((
            ConsensusEpoch(bits >> Self::ROUND_BITS),
            (bits & ((1 << Self::ROUND_BITS) - 1)) as u32,
        ))
    }

    /// RAI: the epoch a replica must at least be in to vote in this instance:
    /// the instance's epoch, or the one after the epoch a close round closes
    pub fn required_epoch(&self) -> ConsensusEpoch {
        match self.as_close_round() {
            Some((epoch, _)) => epoch.next(),
            None => *self,
        }
    }

    const CLOSE_FLAG: u64 = 1 << 63;
    const ROUND_BITS: u32 = 16;

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

    #[test]
    fn close_round_encoding() {
        let epoch = ConsensusEpoch::new(5);
        let round = ConsensusEpoch::close_round(epoch, 3);
        assert!(round.is_close_round());
        assert!(!epoch.is_close_round());
        assert_eq!(round.as_close_round(), Some((epoch, 3)));
        assert_eq!(epoch.as_close_round(), None);
        assert_eq!(round.required_epoch(), ConsensusEpoch::new(6));
        assert_eq!(epoch.required_epoch(), epoch);
        assert_ne!(
            ConsensusEpoch::close_round(epoch, 0),
            ConsensusEpoch::close_round(epoch, 1)
        );
        assert_ne!(
            ConsensusEpoch::close_round(epoch, 0),
            ConsensusEpoch::close_round(ConsensusEpoch::new(6), 0)
        );
        // A close round sorts after every epoch, a lagging node caches its votes
        assert!(round > ConsensusEpoch::new(u64::MAX >> 1));
    }
}
