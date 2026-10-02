use bitvec::prelude::BitArray;

use rsnano_types::{
    BlockHash, ConsensusEpoch, DeserializationError, PrivateKey, PublicKey, Signature,
};

use crate::MessageVariant;

/// RAI, "Reports that remain reconstructible": the signed report of one
/// replica for one epoch. It binds the epoch, the predecessor checkpoint,
/// the committee, the identity and two roots, and nothing else: no block,
/// no certificate, no vote. The inventories behind the roots may hold
/// millions of entries; this message stays 232 bytes. The session is the
/// network the message header names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub epoch: ConsensusEpoch,
    /// H(O_e): the digest of the committee that issued the epoch's votes
    pub committee: BlockHash,
    /// d_{e-1}: the closed predecessor checkpoint the report is signed
    /// against
    pub predecessor: BlockHash,
    /// r_i: the root of the reporter's certified block tree
    pub certified: BlockHash,
    /// g_i: the root of the vote evidence the certified tree does not
    /// summarize
    pub residual: BlockHash,
    pub reporter: PublicKey,
    pub signature: Signature,
}

impl Report {
    pub const SERIALIZED_SIZE: usize = ConsensusEpoch::SERIALIZED_SIZE
        + BlockHash::SERIALIZED_SIZE * 4
        + PublicKey::SERIALIZED_SIZE
        + Signature::SERIALIZED_SIZE;

    pub fn new(
        key: &PrivateKey,
        epoch: ConsensusEpoch,
        committee: BlockHash,
        predecessor: BlockHash,
        certified: BlockHash,
        residual: BlockHash,
        payload: BlockHash,
    ) -> Self {
        Self {
            epoch,
            committee,
            predecessor,
            certified,
            residual,
            reporter: key.public_key(),
            signature: key.sign(payload.as_bytes()),
        }
    }

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            committee: BlockHash::from(2),
            predecessor: BlockHash::from(7),
            certified: BlockHash::from(3),
            residual: BlockHash::from(4),
            reporter: PublicKey::from(5),
            signature: Signature::from_bytes([6; 64]),
        }
    }

    /// Lemma 6.1: the reporter's signature binds the epoch, the committee and
    /// the root; `payload` is what was signed, built by the caller from the
    /// same three fields
    pub fn verify(&self, payload: BlockHash) -> bool {
        self.reporter
            .verify(payload.as_bytes(), &self.signature)
            .is_ok()
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.committee.serialize(writer)?;
        self.predecessor.serialize(writer)?;
        self.certified.serialize(writer)?;
        self.residual.serialize(writer)?;
        self.reporter.serialize(writer)?;
        self.signature.serialize(writer)
    }

    pub const fn serialized_size(_extensions: BitArray<u16>) -> usize {
        Self::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        Ok(Self {
            epoch: ConsensusEpoch::deserialize(&mut bytes)?,
            committee: BlockHash::deserialize(&mut bytes)?,
            predecessor: BlockHash::deserialize(&mut bytes)?,
            certified: BlockHash::deserialize(&mut bytes)?,
            residual: BlockHash::deserialize(&mut bytes)?,
            reporter: PublicKey::deserialize(&mut bytes)?,
            signature: Signature::deserialize(&mut bytes)?,
        })
    }
}

impl MessageVariant for Report {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_round_trip() {
        let report = Report::new_test_instance();
        let mut bytes = Vec::new();
        report.serialize(&mut bytes).unwrap();
        assert_eq!(bytes.len(), Report::SERIALIZED_SIZE);
        assert_eq!(Report::deserialize(&bytes).unwrap(), report);
    }
}
