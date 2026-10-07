use bitvec::prelude::BitArray;

use rsnano_types::{
    BlockHash, ConsensusEpoch, DeserializationError, PrivateKey, PublicKey, Signature,
};

use crate::MessageVariant;

/// RAI, "The joint epoch election": one report a proposal selects, by its
/// reporter and the two roots that reporter signed
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReportSelection {
    pub reporter: PublicKey,
    /// r_i, the certified-state root
    pub certified: BlockHash,
    /// g_i, the residual-vote root
    pub residual: BlockHash,
}

impl ReportSelection {
    pub const SERIALIZED_SIZE: usize = PublicKey::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE * 2;

    fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.reporter.serialize(writer)?;
        self.certified.serialize(writer)?;
        self.residual.serialize(writer)
    }

    fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        Ok(Self {
            reporter: PublicKey::deserialize(bytes)?,
            certified: BlockHash::deserialize(bytes)?,
            residual: BlockHash::deserialize(bytes)?,
        })
    }
}

/// RAI, "The joint epoch election": the leader's proposal of an epoch value
/// `X = (h_p, Q_e, d_e)` in one election slot. The value itself is small
/// however large the epoch state is: it names the reports it selects and the
/// hash of the state they determine, and every validator re-derives
/// `BuildState(S_{e-1}, Q_e)` and checks that hash for itself rather than
/// comparing the proposal with a state of its own.
///
/// The signature binds the session, the epoch, the election slot and the
/// value, so a proposal cannot be replayed into another slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochProp {
    pub epoch: ConsensusEpoch,
    /// The epoch-election slot; separate from account slots
    pub slot: u32,
    /// h_p: the parent epoch placement, zero for a child of election genesis
    pub parent: BlockHash,
    /// d_e: the hash of the state the selected reports determine
    pub state: BlockHash,
    /// Q_e, in canonical order
    pub reports: Vec<ReportSelection>,
    pub leader: PublicKey,
    pub signature: Signature,
}

impl EpochProp {
    /// Reports one proposal may select. A committee has a bounded number of
    /// members and a proposal selects N-f of them; the cap only keeps a
    /// malformed message from being unbounded.
    pub const MAX_REPORTS: usize = 64;

    pub fn new(
        key: &PrivateKey,
        epoch: ConsensusEpoch,
        slot: u32,
        parent: BlockHash,
        state: BlockHash,
        reports: Vec<ReportSelection>,
        payload: BlockHash,
    ) -> Self {
        Self {
            epoch,
            slot,
            parent,
            state,
            reports,
            leader: key.public_key(),
            signature: key.sign(payload.as_bytes()),
        }
    }

    pub fn new_test_instance() -> Self {
        Self {
            epoch: ConsensusEpoch::new(1),
            slot: 2,
            parent: BlockHash::from(3),
            state: BlockHash::from(4),
            reports: vec![ReportSelection {
                reporter: PublicKey::from(5),
                certified: BlockHash::from(6),
                residual: BlockHash::from(7),
            }],
            leader: PublicKey::from(8),
            signature: Signature::from_bytes([9; 64]),
        }
    }

    /// The leader's signature binds the value; `payload` is what was signed,
    /// built by the caller from the same fields
    pub fn verify(&self, payload: BlockHash) -> bool {
        self.leader
            .verify(payload.as_bytes(), &self.signature)
            .is_ok()
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.epoch.serialize(writer)?;
        writer.write_all(&self.slot.to_le_bytes())?;
        self.parent.serialize(writer)?;
        self.state.serialize(writer)?;
        self.leader.serialize(writer)?;
        self.signature.serialize(writer)?;
        for report in &self.reports {
            report.serialize(writer)?;
        }
        Ok(())
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        let bytes = &mut bytes;
        let epoch = ConsensusEpoch::deserialize(bytes)?;
        let mut slot = [0u8; 4];
        read_exact(bytes, &mut slot)?;
        let parent = BlockHash::deserialize(bytes)?;
        let state = BlockHash::deserialize(bytes)?;
        let leader = PublicKey::deserialize(bytes)?;
        let signature = Signature::deserialize(bytes)?;
        let mut reports = Vec::new();
        while !bytes.is_empty() {
            if reports.len() >= Self::MAX_REPORTS {
                return Err(DeserializationError::InvalidData);
            }
            reports.push(ReportSelection::deserialize(bytes)?);
        }
        Ok(Self {
            epoch,
            slot: u32::from_le_bytes(slot),
            parent,
            state,
            reports,
            leader,
            signature,
        })
    }
}

impl MessageVariant for EpochProp {
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
    fn serialize_epoch_prop() {
        assert_deserializable(&Message::EpochProp(EpochProp::new_test_instance()));
    }

    #[test]
    fn serialize_an_epoch_prop_with_several_reports() {
        let mut prop = EpochProp::new_test_instance();
        prop.reports = (0..5)
            .map(|i| ReportSelection {
                reporter: PublicKey::from(i + 10),
                certified: BlockHash::from(i + 20),
                residual: BlockHash::from(i + 30),
            })
            .collect();
        assert_deserializable(&Message::EpochProp(prop));
    }

    #[test]
    fn a_proposal_is_signed_over_its_payload() {
        let key = PrivateKey::from(42);
        let payload = BlockHash::from(99);
        let prop = EpochProp::new(
            &key,
            ConsensusEpoch::new(2),
            1,
            BlockHash::from(1),
            BlockHash::from(2),
            Vec::new(),
            payload,
        );
        assert_eq!(prop.leader, key.public_key());
        assert!(prop.verify(payload));
        assert!(!prop.verify(BlockHash::from(100)));
    }
}
