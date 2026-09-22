use bitvec::prelude::BitArray;

use rsnano_types::{
    Account, BlockHash, ConsensusEpoch, DeserializationError, PrivateKey, PublicKey, Signature,
};

use crate::MessageVariant;

/// RAI, "Reports that remain reconstructible": the signed report of one
/// replica for one epoch. It binds the epoch, the predecessor checkpoint,
/// the committee, the identity and two roots, and nothing else: no block,
/// no certificate, no vote. The inventories behind the roots may hold
/// millions of entries; this message stays 208 bytes. The session is the
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

/// RAI, "Reconstruction": a request to reconstruct a historical certified
/// state. The requester names the epoch, the root it wants, and the roots of
/// the states it holds itself; a replica that knows the target and one of
/// those sources answers with the difference between the two.
///
/// There is no other way to obtain a state: no sketch, and no canonical
/// empty source to fall back on. Correct validators keep updating their
/// local inventories as the evidence of a closed epoch arrives, so their
/// roots converge, and a retry then finds a source the requester and a
/// correct reporter share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconReq {
    pub epoch: ConsensusEpoch,
    /// r_t: the root the requester wants to reconstruct
    pub target: BlockHash,
    /// r_s: the roots of the states the requester holds, any of which it can
    /// apply a difference to
    pub sources: Vec<BlockHash>,
}

impl ReconReq {
    /// Source roots in one request: a requester offers the states it signed,
    /// the reports it reconstructed and its live state, and that is few
    pub const MAX_SOURCES: usize = 8;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            target: BlockHash::from(3),
            sources: vec![BlockHash::from(2), BlockHash::from(4)],
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.target.serialize(writer)?;
        for source in &self.sources {
            source.serialize(writer)?;
        }
        Ok(())
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let target = BlockHash::deserialize(bytes)?;
        let mut sources = Vec::new();
        while !bytes.is_empty() {
            if sources.len() >= Self::MAX_SOURCES {
                return Err(DeserializationError::InvalidData);
            }
            sources.push(BlockHash::deserialize(bytes)?);
        }
        if sources.is_empty() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            epoch,
            target,
            sources,
        })
    }
}

impl MessageVariant for ReconReq {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

/// One entry of a certified state on the wire: where the block sits, which
/// block it is, the parent it names, and what the reporter constructed for
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CertifiedEntry {
    pub account: Account,
    pub height: u64,
    pub hash: BlockHash,
    /// The parent the block names; zero when it opens the account
    pub previous: BlockHash,
    /// 0 notarized, 1 finalized, 2 fast finalized
    pub status: u8,
}

impl CertifiedEntry {
    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.account.serialize(writer)?;
        writer.write_all(&self.height.to_le_bytes())?;
        self.hash.serialize(writer)?;
        self.previous.serialize(writer)?;
        writer.write_all(&[self.status])
    }

    fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        let account = Account::deserialize(bytes)?;
        let mut height = [0u8; 8];
        read_exact(bytes, &mut height)?;
        let hash = BlockHash::deserialize(bytes)?;
        let previous = BlockHash::deserialize(bytes)?;
        let mut status = [0u8; 1];
        read_exact(bytes, &mut status)?;
        if status[0] > 2 {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            account,
            height: u64::from_le_bytes(height),
            hash,
            previous,
            status: status[0],
        })
    }
}

/// RAI, "Reconstruction": the canonical edits that take the source state to
/// the target, "which may add, remove, or change the status of records".
/// The requester applies them to its copy of the source and accepts the
/// result exactly when its root comes out as the target, so the reply
/// carries no signature and no proof of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconReply {
    pub epoch: ConsensusEpoch,
    /// r_s: the source the responder took, one of those the request offered
    pub source: BlockHash,
    pub target: BlockHash,
    /// Entries the target holds and the source does not, or holds otherwise
    pub added: Vec<CertifiedEntry>,
    /// Entries the source holds and the target does not
    pub removed: Vec<CertifiedEntry>,
}

impl ReconReply {
    /// Edits in one reply, bounded by the message size: an entry is 105
    /// bytes against a payload of 64 KiB. A difference between two states of
    /// one epoch is the evidence that arrived between them and is small in
    /// the common case; one larger than this is not answered, and the
    /// requester reconstructs from a later shared state instead.
    pub const MAX_ENTRIES: usize = 600;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            source: BlockHash::from(2),
            target: BlockHash::from(3),
            added: vec![CertifiedEntry {
                account: Account::from(4),
                height: 5,
                hash: BlockHash::from(6),
                previous: BlockHash::from(7),
                status: 1,
            }],
            removed: vec![CertifiedEntry {
                account: Account::from(8),
                height: 9,
                hash: BlockHash::from(10),
                previous: BlockHash::from(11),
                status: 0,
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
        writer.write_all(&(self.added.len() as u16).to_le_bytes())?;
        for entry in &self.added {
            entry.serialize(writer)?;
        }
        for entry in &self.removed {
            entry.serialize(writer)?;
        }
        Ok(())
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let source = BlockHash::deserialize(bytes)?;
        let target = BlockHash::deserialize(bytes)?;
        let mut added_len = [0u8; 2];
        read_exact(bytes, &mut added_len)?;
        let added_len = u16::from_le_bytes(added_len) as usize;
        if added_len > Self::MAX_ENTRIES {
            return Err(DeserializationError::InvalidData);
        }
        let mut added = Vec::with_capacity(added_len);
        for _ in 0..added_len {
            added.push(CertifiedEntry::deserialize(bytes)?);
        }
        let mut removed = Vec::new();
        while !bytes.is_empty() {
            if added.len() + removed.len() >= Self::MAX_ENTRIES {
                return Err(DeserializationError::InvalidData);
            }
            removed.push(CertifiedEntry::deserialize(bytes)?);
        }
        Ok(Self {
            epoch,
            source,
            target,
            added,
            removed,
        })
    }
}

impl MessageVariant for ReconReply {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

/// One entry of a residual-vote object on the wire: the block the reporter
/// voted for, which of its own votes it recorded, and its signature over
/// the record
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidualEntry {
    pub account: Account,
    pub height: u64,
    pub hash: BlockHash,
    /// The parent the block names; zero when it opens the account
    pub previous: BlockHash,
    /// 0 first, 1 notarization support, 2 final
    pub kind: u8,
    pub signature: Signature,
}

impl ResidualEntry {
    pub const SERIALIZED_SIZE: usize = Account::SERIALIZED_SIZE
        + 8
        + BlockHash::SERIALIZED_SIZE * 2
        + 1
        + Signature::SERIALIZED_SIZE;

    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.account.serialize(writer)?;
        writer.write_all(&self.height.to_le_bytes())?;
        self.hash.serialize(writer)?;
        self.previous.serialize(writer)?;
        writer.write_all(&[self.kind])?;
        self.signature.serialize(writer)
    }

    fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        let account = Account::deserialize(bytes)?;
        let mut height = [0u8; 8];
        read_exact(bytes, &mut height)?;
        let hash = BlockHash::deserialize(bytes)?;
        let previous = BlockHash::deserialize(bytes)?;
        let mut kind = [0u8; 1];
        read_exact(bytes, &mut kind)?;
        if kind[0] > 2 {
            return Err(DeserializationError::InvalidData);
        }
        let signature = Signature::deserialize(bytes)?;
        Ok(Self {
            account,
            height: u64::from_le_bytes(height),
            hash,
            previous,
            kind: kind[0],
            signature,
        })
    }
}

/// RAI, "Residual evidence and retention": a request for the residual object
/// a report committed to with `g_i`. Unlike a certified state, a residual
/// object holds one reporter's own votes and never changes, so there is no
/// difference to take against a state of the requester's: it is fetched
/// whole, in canonical order, from the offset the requester already holds.
/// Anyone that has reconstructed it serves it, not only the reporter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidualReq {
    pub epoch: ConsensusEpoch,
    /// g_i: the root the reporter signed
    pub root: BlockHash,
    /// Entries of the object the requester already holds
    pub offset: u32,
}

impl ResidualReq {
    pub const SERIALIZED_SIZE: usize =
        ConsensusEpoch::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 4;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            root: BlockHash::from(2),
            offset: 3,
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.root.serialize(writer)?;
        writer.write_all(&self.offset.to_le_bytes())
    }

    pub const fn serialized_size(_extensions: BitArray<u16>) -> usize {
        Self::SERIALIZED_SIZE
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let epoch = ConsensusEpoch::deserialize(&mut bytes)?;
        let root = BlockHash::deserialize(&mut bytes)?;
        let mut offset = [0u8; 4];
        read_exact(&mut bytes, &mut offset)?;
        Ok(Self {
            epoch,
            root,
            offset: u32::from_le_bytes(offset),
        })
    }
}

impl MessageVariant for ResidualReq {}

/// RAI: part of a residual object, in canonical order, from the offset the
/// request named. The requester accepts the object exactly when what it has
/// accumulated hashes to `g_i`, so the reply carries no signature and no
/// proof of its own. The offset is what lets the requester take the parts in
/// order whatever order the answers of different replicas arrive in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidualReply {
    pub epoch: ConsensusEpoch,
    pub root: BlockHash,
    /// Entries of the object before the first one carried
    pub offset: u32,
    pub entries: Vec<ResidualEntry>,
}

impl ResidualReply {
    /// Entries in one reply, bounded by the message size: a signed entry is
    /// 137 bytes against a payload of 64 KiB
    pub const MAX_ENTRIES: usize = 450;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            root: BlockHash::from(2),
            offset: 3,
            entries: vec![ResidualEntry {
                account: Account::from(3),
                height: 4,
                hash: BlockHash::from(5),
                previous: BlockHash::from(6),
                kind: 1,
                signature: Signature::from_bytes([8; 64]),
            }],
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.root.serialize(writer)?;
        writer.write_all(&self.offset.to_le_bytes())?;
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
        let root = BlockHash::deserialize(&mut bytes)?;
        let mut offset = [0u8; 4];
        read_exact(&mut bytes, &mut offset)?;
        let mut entries = Vec::new();
        while !bytes.is_empty() {
            if entries.len() >= Self::MAX_ENTRIES {
                return Err(DeserializationError::InvalidData);
            }
            entries.push(ResidualEntry::deserialize(&mut bytes)?);
        }
        Ok(Self {
            epoch,
            root,
            offset: u32::from_le_bytes(offset),
            entries,
        })
    }
}

impl MessageVariant for ResidualReply {
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
            BlockHash::from(4),
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
    fn serialize_residual_req() {
        assert_deserializable(&Message::ResidualReq(ResidualReq::new_test_instance()));
    }

    #[test]
    fn serialize_residual_reply() {
        assert_deserializable(&Message::ResidualReply(ResidualReply::new_test_instance()));
        let empty = ResidualReply {
            entries: Vec::new(),
            ..ResidualReply::new_test_instance()
        };
        assert_deserializable(&Message::ResidualReply(empty));
    }

    #[test]
    fn a_residual_reply_with_an_unknown_kind_is_refused() {
        let mut bytes = Vec::new();
        ResidualReply::new_test_instance()
            .serialize(&mut bytes)
            .unwrap();
        // The kind precedes the signature of the only entry
        let kind = bytes.len() - Signature::SERIALIZED_SIZE - 1;
        bytes[kind] = 7;
        assert!(ResidualReply::deserialize(&bytes).is_err());
    }

    #[test]
    fn a_reply_with_an_unknown_status_is_refused() {
        let mut reply = ReconReply::new_test_instance();
        reply.removed.clear();
        let mut bytes = Vec::new();
        reply.serialize(&mut bytes).unwrap();
        // The status is the last byte of the only entry
        *bytes.last_mut().unwrap() = 9;
        assert!(ReconReply::deserialize(&bytes).is_err());
    }

    #[test]
    fn serialize_a_recon_reply_with_no_edits() {
        let reply = ReconReply {
            added: Vec::new(),
            removed: Vec::new(),
            ..ReconReply::new_test_instance()
        };
        assert_deserializable(&Message::ReconReply(reply));
    }

    #[test]
    fn a_recon_req_names_at_least_one_source() {
        let request = ReconReq {
            sources: Vec::new(),
            ..ReconReq::new_test_instance()
        };
        let mut bytes = Vec::new();
        request.serialize(&mut bytes).unwrap();
        assert!(ReconReq::deserialize(&bytes).is_err());
    }
}
