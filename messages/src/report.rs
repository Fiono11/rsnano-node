use bitvec::prelude::BitArray;

use rsnano_types::{
    Account, BlockHash, ConsensusEpoch, DeserializationError, PrivateKey, PublicKey, Signature,
};

use crate::MessageVariant;

/// RAI, Section 6.1: the report of one replica for one epoch,
/// `Sign_i(REPORT, e, H(O_e), r_{i,e})`. It carries the root of the
/// reporter's map of first and final votes and nothing else: no block, no
/// certificate, no vote signature. The map behind it may hold millions of
/// entries; this message stays 176 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub epoch: ConsensusEpoch,
    /// H(O_e): the digest of the committee that issued the epoch's votes
    pub committee: BlockHash,
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
        + BlockHash::SERIALIZED_SIZE * 3
        + PublicKey::SERIALIZED_SIZE
        + Signature::SERIALIZED_SIZE;

    pub fn new(
        key: &PrivateKey,
        epoch: ConsensusEpoch,
        committee: BlockHash,
        certified: BlockHash,
        residual: BlockHash,
        payload: BlockHash,
    ) -> Self {
        Self {
            epoch,
            committee,
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
            certified: BlockHash::deserialize(&mut bytes)?,
            residual: BlockHash::deserialize(&mut bytes)?,
            reporter: PublicKey::deserialize(&mut bytes)?,
            signature: Signature::deserialize(&mut bytes)?,
        })
    }
}

impl MessageVariant for Report {}

/// RAI, "Reconciliation": a request to reconstruct a historical certified
/// state. The requester names the epoch, the state it currently holds and
/// the target it wants; a replica answers only if it knows both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconReq {
    pub epoch: ConsensusEpoch,
    /// r_s: the root of the certified state the requester holds now
    pub source: BlockHash,
    /// r_t: the root it wants to reconstruct
    pub target: BlockHash,
}

impl ReconReq {
    pub const SERIALIZED_SIZE: usize =
        ConsensusEpoch::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE * 2;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            source: BlockHash::from(2),
            target: BlockHash::from(3),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.source.serialize(writer)?;
        self.target.serialize(writer)
    }

    pub const fn serialized_size(_extensions: BitArray<u16>) -> usize {
        Self::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        Ok(Self {
            epoch: ConsensusEpoch::deserialize(&mut bytes)?,
            source: BlockHash::deserialize(&mut bytes)?,
            target: BlockHash::deserialize(&mut bytes)?,
        })
    }
}

impl MessageVariant for ReconReq {}

/// One entry of a certified state on the wire: where the block sits, which
/// block it is, and what the reporter had constructed for it
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CertifiedEntry {
    pub account: Account,
    pub height: u64,
    pub hash: BlockHash,
    /// 0 notarized, 1 finalized, 2 fast finalized
    pub status: u8,
}

impl CertifiedEntry {
    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.account.serialize(writer)?;
        writer.write_all(&self.height.to_le_bytes())?;
        self.hash.serialize(writer)?;
        writer.write_all(&[self.status])
    }

    fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        let account = Account::deserialize(bytes)?;
        let mut height = [0u8; 8];
        read_exact(bytes, &mut height)?;
        let hash = BlockHash::deserialize(bytes)?;
        let mut status = [0u8; 1];
        read_exact(bytes, &mut status)?;
        if status[0] > 2 {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            account,
            height: u64::from_le_bytes(height),
            hash,
            status: status[0],
        })
    }
}

/// RAI, "Reconciliation": the reconstructive difference from the source
/// state to the target. The requester applies it and accepts the result
/// exactly when the root comes out as the target, so the reply carries no
/// signature and no proof of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconReply {
    pub epoch: ConsensusEpoch,
    pub source: BlockHash,
    pub target: BlockHash,
    pub entries: Vec<CertifiedEntry>,
}

impl ReconReply {
    /// Entries in one reply. A difference between two states of the same
    /// epoch is the evidence that arrived between them, so it is small in
    /// the common case; a reply that would exceed this is not sent, and the
    /// requester reconstructs from a later common state instead.
    pub const MAX_ENTRIES: usize = 700;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            source: BlockHash::from(2),
            target: BlockHash::from(3),
            entries: vec![CertifiedEntry {
                account: Account::from(4),
                height: 5,
                hash: BlockHash::from(6),
                status: 1,
            }],
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.source.serialize(writer)?;
        self.target.serialize(writer)?;
        for entry in &self.entries {
            entry.serialize(writer)?;
        }
        Ok(())
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let epoch = ConsensusEpoch::deserialize(&mut bytes)?;
        let source = BlockHash::deserialize(&mut bytes)?;
        let target = BlockHash::deserialize(&mut bytes)?;
        let mut entries = Vec::new();
        while !bytes.is_empty() {
            entries.push(CertifiedEntry::deserialize(&mut bytes)?);
        }
        Ok(Self {
            epoch,
            source,
            target,
            entries,
        })
    }
}

impl MessageVariant for ReconReply {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

fn read_exact(bytes: &mut &[u8], buffer: &mut [u8]) -> Result<(), DeserializationError> {
    if bytes.len() < buffer.len() {
        return Err(DeserializationError::InvalidData);
    }
    let (head, tail) = bytes.split_at(buffer.len());
    buffer.copy_from_slice(head);
    *bytes = tail;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn serialize_report() {
        assert_deserializable(&Message::Report(Report::new_test_instance()));
    }

    #[test]
    fn a_report_is_signed_over_its_payload() {
        let key = PrivateKey::from(42);
        let payload = BlockHash::from(99);
        let report = Report::new(
            &key,
            ConsensusEpoch::new(2),
            BlockHash::from(1),
            BlockHash::from(2),
            BlockHash::from(3),
            payload,
        );
        assert_eq!(report.reporter, key.public_key());
        assert!(report.verify(payload));
        assert!(!report.verify(BlockHash::from(100)));
    }

    #[test]
    fn serialize_recon_req() {
        assert_deserializable(&Message::ReconReq(ReconReq::new_test_instance()));
    }

    #[test]
    fn serialize_recon_reply() {
        assert_deserializable(&Message::ReconReply(ReconReply::new_test_instance()));
    }

    #[test]
    fn serialize_an_empty_recon_reply() {
        let reply = ReconReply {
            entries: Vec::new(),
            ..ReconReply::new_test_instance()
        };
        assert_deserializable(&Message::ReconReply(reply));
    }

    #[test]
    fn a_reply_with_an_unknown_status_is_refused() {
        let mut bytes = Vec::new();
        ReconReply::new_test_instance()
            .serialize(&mut bytes)
            .unwrap();
        // The status is the last byte of the only entry
        *bytes.last_mut().unwrap() = 7;
        assert!(ReconReply::deserialize(&bytes).is_err());
    }
}
