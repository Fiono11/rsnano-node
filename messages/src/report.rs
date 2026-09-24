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
    /// 0 notarized, 1 finalized
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

    fn deserialize(bytes: &mut &[u8], max_status: u8) -> Result<Self, DeserializationError> {
        let account = Account::deserialize(bytes)?;
        let mut height = [0u8; 8];
        read_exact(bytes, &mut height)?;
        let hash = BlockHash::deserialize(bytes)?;
        let previous = BlockHash::deserialize(bytes)?;
        let mut status = [0u8; 1];
        read_exact(bytes, &mut status)?;
        if status[0] > max_status {
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
    pub page: u16,
    pub pages: u16,
    /// r_s: the source the responder took, one of those the request offered
    pub source: BlockHash,
    pub target: BlockHash,
    /// Entries the target holds and the source does not, or holds otherwise
    pub added: Vec<CertifiedEntry>,
    /// Entries the source holds and the target does not
    pub removed: Vec<CertifiedEntry>,
}

impl ReconReply {
    /// Edits per page, bounded by the 64 KiB message payload. The source and
    /// target roots identify one immutable difference across all pages.
    pub const MAX_ENTRIES: usize = 600;
    /// Bound total buffered edits independently of untrusted page counts.
    pub const MAX_PAGES: usize = 256;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            page: 0,
            pages: 1,
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
        writer.write_all(&self.page.to_le_bytes())?;
        writer.write_all(&self.pages.to_le_bytes())?;
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

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DeserializationError> {
        Self::deserialize_with_status(bytes, 2)
    }

    pub(crate) fn deserialize_with_status(
        mut bytes: &[u8],
        max_status: u8,
    ) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let mut value = [0u8; 2];
        read_exact(bytes, &mut value)?;
        let page = u16::from_le_bytes(value);
        read_exact(bytes, &mut value)?;
        let pages = u16::from_le_bytes(value);
        if pages == 0 || pages as usize > Self::MAX_PAGES || page >= pages {
            return Err(DeserializationError::InvalidData);
        }
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
            added.push(CertifiedEntry::deserialize(bytes, max_status)?);
        }
        let mut removed = Vec::new();
        while !bytes.is_empty() {
            if added.len() + removed.len() >= Self::MAX_ENTRIES {
                return Err(DeserializationError::InvalidData);
            }
            removed.push(CertifiedEntry::deserialize(bytes, max_status)?);
        }
        Ok(Self {
            epoch,
            page,
            pages,
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

/// One cell of a set sketch on the wire
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SketchCellWire {
    pub count: i32,
    pub key: [u8; 32],
    pub check: u32,
}

impl SketchCellWire {
    pub const SERIALIZED_SIZE: usize = 4 + 32 + 4;

    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&self.count.to_le_bytes())?;
        writer.write_all(&self.key)?;
        writer.write_all(&self.check.to_le_bytes())
    }

    fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        let mut count = [0u8; 4];
        read_exact(bytes, &mut count)?;
        let mut key = [0u8; 32];
        read_exact(bytes, &mut key)?;
        let mut check = [0u8; 4];
        read_exact(bytes, &mut check)?;
        Ok(Self {
            count: i32::from_le_bytes(count),
            key,
            check: u32::from_le_bytes(check),
        })
    }
}

/// RAI, "Reconstructing a report": the authenticated difference from the
/// requester's epoch-e tagged ledger set to a frozen `T_i`, asked for by a
/// set sketch when no root the requester holds is known to any responder.
/// The requester sketches the digests of its tagged entries; a replica that
/// holds the target subtracts its own sketch of the target and peels the
/// symmetric difference out. The reply is checked against the signed root
/// like every other difference: the sketch is an encoding of the request,
/// not a trust path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerSketchReq {
    pub epoch: ConsensusEpoch,
    /// r_t: the signed root wanted
    pub target: BlockHash,
    /// The root of the requester's snapshot the sketch describes; the reply
    /// names it so that the edits are applied to that snapshot
    pub source: BlockHash,
    pub cells: Vec<SketchCellWire>,
}

impl LedgerSketchReq {
    /// Cells in one request: 40 KiB on the wire
    pub const MAX_CELLS: usize = 1024;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            target: BlockHash::from(2),
            source: BlockHash::from(3),
            cells: vec![SketchCellWire {
                count: -1,
                key: [7; 32],
                check: 9,
            }],
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.target.serialize(writer)?;
        self.source.serialize(writer)?;
        for cell in &self.cells {
            cell.serialize(writer)?;
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
        let source = BlockHash::deserialize(bytes)?;
        let mut cells = Vec::new();
        while !bytes.is_empty() {
            if cells.len() >= Self::MAX_CELLS {
                return Err(DeserializationError::InvalidData);
            }
            cells.push(SketchCellWire::deserialize(bytes)?);
        }
        if cells.is_empty() {
            return Err(DeserializationError::InvalidData);
        }
        Ok(Self {
            epoch,
            target,
            source,
            cells,
        })
    }
}

impl MessageVariant for LedgerSketchReq {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

/// RAI: what a frozen `T_i` and the requester's snapshot differ in, as the
/// responder peeled it out of the sketch: the tagged entries the requester
/// lacks, and the digests of entries it holds that `T_i` does not. Paged
/// like a root-based difference; no partial transfer is usable. Or nothing
/// but `incomplete`, when the difference did not peel out of a sketch that
/// size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerSketchReply {
    pub epoch: ConsensusEpoch,
    pub target: BlockHash,
    pub source: BlockHash,
    pub incomplete: bool,
    pub page: u16,
    pub pages: u16,
    pub added: Vec<CertifiedEntry>,
    pub removed: Vec<BlockHash>,
}

impl LedgerSketchReply {
    /// Edits in one page: 105 bytes an added entry, 32 a removed digest,
    /// against a payload of 64 KiB
    pub const MAX_ENTRIES: usize = 400;
    pub const MAX_PAGES: usize = ReconReply::MAX_PAGES;

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            target: BlockHash::from(2),
            source: BlockHash::from(3),
            incomplete: false,
            page: 0,
            pages: 1,
            added: vec![CertifiedEntry {
                account: Account::from(3),
                height: 4,
                hash: BlockHash::from(5),
                previous: BlockHash::from(6),
                status: 1,
            }],
            removed: vec![BlockHash::from(8)],
        }
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        self.target.serialize(writer)?;
        self.source.serialize(writer)?;
        writer.write_all(&[u8::from(self.incomplete)])?;
        writer.write_all(&self.page.to_le_bytes())?;
        writer.write_all(&self.pages.to_le_bytes())?;
        writer.write_all(&(self.added.len() as u16).to_le_bytes())?;
        for entry in &self.added {
            entry.serialize(writer)?;
        }
        for digest in &self.removed {
            digest.serialize(writer)?;
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
        let source = BlockHash::deserialize(bytes)?;
        let mut incomplete = [0u8; 1];
        read_exact(bytes, &mut incomplete)?;
        if incomplete[0] > 1 {
            return Err(DeserializationError::InvalidData);
        }
        let mut value = [0u8; 2];
        read_exact(bytes, &mut value)?;
        let page = u16::from_le_bytes(value);
        read_exact(bytes, &mut value)?;
        let pages = u16::from_le_bytes(value);
        if pages == 0 || pages as usize > Self::MAX_PAGES || page >= pages {
            return Err(DeserializationError::InvalidData);
        }
        read_exact(bytes, &mut value)?;
        let added_len = u16::from_le_bytes(value) as usize;
        if added_len > Self::MAX_ENTRIES {
            return Err(DeserializationError::InvalidData);
        }
        let mut added = Vec::with_capacity(added_len);
        for _ in 0..added_len {
            added.push(CertifiedEntry::deserialize(bytes, 2)?);
        }
        let mut removed = Vec::new();
        while !bytes.is_empty() {
            if added.len() + removed.len() >= Self::MAX_ENTRIES {
                return Err(DeserializationError::InvalidData);
            }
            removed.push(BlockHash::deserialize(bytes)?);
        }
        Ok(Self {
            epoch,
            target,
            source,
            incomplete: incomplete[0] == 1,
            page,
            pages,
            added,
            removed,
        })
    }
}

impl MessageVariant for LedgerSketchReply {
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
    fn reconstruction_page_indices_are_bounded() {
        for (page, pages) in [(0, 0), (2, 2), (0, ReconReply::MAX_PAGES as u16 + 1)] {
            let reply = ReconReply {
                page,
                pages,
                ..ReconReply::new_test_instance()
            };
            let mut bytes = Vec::new();
            reply.serialize(&mut bytes).unwrap();
            assert!(ReconReply::deserialize(&bytes).is_err());
        }
        let reply = ReconReply {
            page: 1,
            pages: 2,
            ..ReconReply::new_test_instance()
        };
        let mut bytes = Vec::new();
        reply.serialize(&mut bytes).unwrap();
        assert_eq!(ReconReply::deserialize(&bytes).unwrap(), reply);
    }

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
    fn serialize_ledger_sketch_req() {
        assert_deserializable(&Message::LedgerSketchReq(
            LedgerSketchReq::new_test_instance(),
        ));
        let cells = (0..200)
            .map(|i| SketchCellWire {
                count: i as i32 - 100,
                key: [i as u8; 32],
                check: i,
            })
            .collect();
        assert_deserializable(&Message::LedgerSketchReq(LedgerSketchReq {
            cells,
            ..LedgerSketchReq::new_test_instance()
        }));
    }

    #[test]
    fn serialize_ledger_sketch_reply() {
        assert_deserializable(&Message::LedgerSketchReply(
            LedgerSketchReply::new_test_instance(),
        ));
        assert_deserializable(&Message::LedgerSketchReply(LedgerSketchReply {
            incomplete: true,
            added: Vec::new(),
            removed: Vec::new(),
            ..LedgerSketchReply::new_test_instance()
        }));
    }

    #[test]
    fn a_ledger_sketch_reply_with_an_unknown_status_or_page_is_refused() {
        let mut reply = LedgerSketchReply::new_test_instance();
        reply.removed.clear();
        let mut bytes = Vec::new();
        reply.serialize(&mut bytes).unwrap();
        // The status is the last byte of the only entry
        *bytes.last_mut().unwrap() = 7;
        assert!(LedgerSketchReply::deserialize(&bytes).is_err());
        let mut bytes = Vec::new();
        LedgerSketchReply {
            page: 1,
            pages: 1,
            ..LedgerSketchReply::new_test_instance()
        }
        .serialize(&mut bytes)
        .unwrap();
        assert!(LedgerSketchReply::deserialize(&bytes).is_err());
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
            page: 0,
            pages: 1,
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
