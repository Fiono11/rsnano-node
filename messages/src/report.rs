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
    /// The root of the reporter's authenticated map
    pub root: BlockHash,
    pub reporter: PublicKey,
    pub signature: Signature,
}

impl Report {
    pub const SERIALIZED_SIZE: usize = ConsensusEpoch::SERIALIZED_SIZE
        + BlockHash::SERIALIZED_SIZE * 2
        + PublicKey::SERIALIZED_SIZE
        + Signature::SERIALIZED_SIZE;

    pub fn new(
        key: &PrivateKey,
        epoch: ConsensusEpoch,
        committee: BlockHash,
        root: BlockHash,
        payload: BlockHash,
    ) -> Self {
        Self {
            epoch,
            committee,
            root,
            reporter: key.public_key(),
            signature: key.sign(payload.as_bytes()),
        }
    }

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            committee: BlockHash::from(2),
            root: BlockHash::from(3),
            reporter: PublicKey::from(4),
            signature: Signature::from_bytes([5; 64]),
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
        self.root.serialize(writer)?;
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
            root: BlockHash::deserialize(&mut bytes)?,
            reporter: PublicKey::deserialize(&mut bytes)?,
            signature: Signature::deserialize(&mut bytes)?,
        })
    }
}

impl MessageVariant for Report {}

/// Which part of a report map a request asks for, and an answer carries
/// (Section 6.2: compare the roots, then the bucket digests, then fetch the
/// entries of the buckets that differ)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportPart {
    /// The digests of the 256 buckets
    Digests,
    /// The entries of one bucket, whole
    Bucket(u8),
}

impl ReportPart {
    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        match self {
            ReportPart::Digests => writer.write_all(&[0, 0]),
            ReportPart::Bucket(bucket) => writer.write_all(&[1, *bucket]),
        }
    }

    fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        let mut buffer = [0u8; 2];
        read_exact(bytes, &mut buffer)?;
        match buffer[0] {
            0 => Ok(ReportPart::Digests),
            1 => Ok(ReportPart::Bucket(buffer[1])),
            _ => Err(DeserializationError::InvalidData),
        }
    }

    const SERIALIZED_SIZE: usize = 2;
}

/// RAI, Section 6.2: a step of a reconciliation against a signed report root.
/// The requester names the report (epoch and reporter) and the part of the
/// map it needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportReq {
    pub epoch: ConsensusEpoch,
    pub reporter: PublicKey,
    pub part: ReportPart,
}

impl ReportReq {
    pub const SERIALIZED_SIZE: usize =
        ConsensusEpoch::SERIALIZED_SIZE + PublicKey::SERIALIZED_SIZE + ReportPart::SERIALIZED_SIZE;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            reporter: PublicKey::from(2),
            part: ReportPart::Bucket(3),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.reporter.serialize(writer)?;
        self.part.serialize(writer)
    }

    pub const fn serialized_size(_extensions: BitArray<u16>) -> usize {
        Self::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        Ok(Self {
            epoch: ConsensusEpoch::deserialize(&mut bytes)?,
            reporter: PublicKey::deserialize(&mut bytes)?,
            part: ReportPart::deserialize(&mut bytes)?,
        })
    }
}

impl MessageVariant for ReportReq {}

/// One entry of a report map on the wire: the slot, which of the two
/// one-shot votes it is, and the block voted for
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportEntry {
    pub account: Account,
    pub height: u64,
    /// false: the first vote of the slot; true: the final vote
    pub is_final: bool,
    pub hash: BlockHash,
}

impl ReportEntry {
    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.account.serialize(writer)?;
        writer.write_all(&self.height.to_le_bytes())?;
        writer.write_all(&[self.is_final as u8])?;
        self.hash.serialize(writer)
    }

    fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        let account = Account::deserialize(bytes)?;
        let mut height = [0u8; 8];
        read_exact(bytes, &mut height)?;
        let mut is_final = [0u8; 1];
        read_exact(bytes, &mut is_final)?;
        Ok(Self {
            account,
            height: u64::from_le_bytes(height),
            is_final: is_final[0] != 0,
            hash: BlockHash::deserialize(bytes)?,
        })
    }
}

/// RAI, Section 6.2: the answer to a `ReportReq`, either the digests of a
/// level or the entries of a leaf bucket. The requester checks every entry
/// against its key and the reconstructed root against the signed one, so an
/// answer needs no signature of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportPayload {
    Digests(Vec<[u8; 32]>),
    Entries(Vec<ReportEntry>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportAck {
    pub epoch: ConsensusEpoch,
    pub reporter: PublicKey,
    pub part: ReportPart,
    pub payload: ReportPayload,
}

impl ReportAck {
    /// One digest per bucket
    pub const BUCKETS: usize = 256;
    /// Entries of one bucket. A report of the benchmark holds ~31000 entries,
    /// so a bucket holds ~120; this is the cap that keeps an answer inside
    /// one message, and a reporter with a larger map is refused rather than
    /// answered with a part of a bucket.
    pub const MAX_ENTRIES: usize = 800;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            reporter: PublicKey::from(2),
            part: ReportPart::Bucket(3),
            payload: ReportPayload::Entries(vec![ReportEntry {
                account: Account::from(5),
                height: 6,
                is_final: true,
                hash: BlockHash::from(7),
            }]),
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.reporter.serialize(writer)?;
        self.part.serialize(writer)?;
        match &self.payload {
            ReportPayload::Digests(digests) => {
                for digest in digests {
                    writer.write_all(digest)?;
                }
            }
            ReportPayload::Entries(entries) => {
                for entry in entries {
                    entry.serialize(writer)?;
                }
            }
        }
        Ok(())
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let epoch = ConsensusEpoch::deserialize(&mut bytes)?;
        let reporter = PublicKey::deserialize(&mut bytes)?;
        let part = ReportPart::deserialize(&mut bytes)?;
        let payload = match part {
            ReportPart::Bucket(_) => {
                let mut entries = Vec::new();
                while !bytes.is_empty() {
                    entries.push(ReportEntry::deserialize(&mut bytes)?);
                }
                ReportPayload::Entries(entries)
            }
            ReportPart::Digests => {
                let mut digests = Vec::new();
                while !bytes.is_empty() {
                    let mut digest = [0u8; 32];
                    read_exact(&mut bytes, &mut digest)?;
                    digests.push(digest);
                }
                if digests.len() != Self::BUCKETS {
                    return Err(DeserializationError::InvalidData);
                }
                ReportPayload::Digests(digests)
            }
        };
        Ok(Self {
            epoch,
            reporter,
            part,
            payload,
        })
    }
}

impl MessageVariant for ReportAck {
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
            payload,
        );
        assert_eq!(report.reporter, key.public_key());
        assert!(report.verify(payload));
        assert!(!report.verify(BlockHash::from(100)));
    }

    #[test]
    fn serialize_report_req() {
        for part in [ReportPart::Digests, ReportPart::Bucket(7)] {
            let req = ReportReq {
                part,
                ..ReportReq::new_test_instance()
            };
            assert_deserializable(&Message::ReportReq(req));
        }
    }

    #[test]
    fn serialize_report_ack_with_entries() {
        assert_deserializable(&Message::ReportAck(ReportAck::new_test_instance()));
    }

    #[test]
    fn serialize_report_ack_with_digests() {
        let ack = ReportAck {
            part: ReportPart::Digests,
            payload: ReportPayload::Digests(
                (0..ReportAck::BUCKETS).map(|i| [i as u8; 32]).collect(),
            ),
            ..ReportAck::new_test_instance()
        };
        assert_deserializable(&Message::ReportAck(ack));
    }

    #[test]
    fn an_ack_of_digests_must_hold_one_per_bucket() {
        let ack = ReportAck {
            part: ReportPart::Digests,
            payload: ReportPayload::Digests(vec![[0; 32]; 3]),
            ..ReportAck::new_test_instance()
        };
        let mut bytes = Vec::new();
        ack.serialize(&mut bytes).unwrap();
        assert!(ReportAck::deserialize(&bytes).is_err());
    }
}
