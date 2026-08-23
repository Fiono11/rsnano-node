use bitvec::array::BitArray;
use rsnano_types::{
    Blake2Hash, Blake2HashBuilder, BlockHash, DeserializationError, PrivateKey, PublicKey,
    QualifiedRoot, Root, Signature, read_u64_be,
};

use crate::MessageVariant;

const DOMAIN: &[u8] = b"RAI/CloseReport";
const FIXED_SIZE: usize = 8 + PublicKey::SERIALIZED_SIZE + Signature::SERIALIZED_SIZE;
const ROOT_SIZE: usize = Root::SERIALIZED_SIZE + BlockHash::SERIALIZED_SIZE + 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseReport {
    pub epoch: u64,
    pub pending: Vec<QualifiedRoot>,
    pub reporter: PublicKey,
    pub signature: Signature,
}

impl CloseReport {
    pub fn new(
        epoch: u64,
        pending: impl IntoIterator<Item = QualifiedRoot>,
        key: &PrivateKey,
    ) -> Self {
        let mut pending: Vec<_> = pending.into_iter().collect();
        pending.sort();
        pending.dedup();
        let reporter = key.public_key();
        let hash = report_hash(epoch, &pending, &reporter);
        Self {
            epoch,
            pending,
            reporter,
            signature: key.sign(hash.as_bytes()),
        }
    }

    pub fn validate(&self) -> bool {
        self.pending.windows(2).all(|roots| roots[0] < roots[1])
            && self
                .reporter
                .verify(
                    report_hash(self.epoch, &self.pending, &self.reporter).as_bytes(),
                    &self.signature,
                )
                .is_ok()
    }

    pub fn hash(&self) -> Blake2Hash {
        report_hash(self.epoch, &self.pending, &self.reporter)
    }

    pub fn serialize<T: std::io::Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&self.epoch.to_be_bytes())?;
        self.reporter.serialize(writer)?;
        self.signature.serialize(writer)?;
        for root in &self.pending {
            writer.write_all(root.root.as_bytes())?;
            writer.write_all(root.previous.as_bytes())?;
            writer.write_all(&root.epoch.to_be_bytes())?;
        }
        Ok(())
    }

    pub fn deserialize(mut bytes: &[u8]) -> Result<Self, DeserializationError> {
        if bytes.len() < FIXED_SIZE || (bytes.len() - FIXED_SIZE) % ROOT_SIZE != 0 {
            return Err(DeserializationError::InvalidData);
        }
        let epoch = read_u64_be(&mut bytes)?;
        let reporter = PublicKey::deserialize(&mut bytes)?;
        let signature = Signature::deserialize(&mut bytes)?;
        let mut pending = Vec::with_capacity(bytes.len() / ROOT_SIZE);
        while !bytes.is_empty() {
            let root = Root::deserialize(&mut bytes)?;
            let previous = BlockHash::deserialize(&mut bytes)?;
            let root_epoch = read_u64_be(&mut bytes)?;
            pending.push(QualifiedRoot::new(root, previous).with_epoch(root_epoch));
        }
        Ok(Self {
            epoch,
            pending,
            reporter,
            signature,
        })
    }

    pub const fn serialized_size(extensions: BitArray<u16>) -> usize {
        extensions.data as usize
    }
}

impl MessageVariant for CloseReport {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

fn report_hash(epoch: u64, pending: &[QualifiedRoot], reporter: &PublicKey) -> Blake2Hash {
    let mut builder = Blake2HashBuilder::default()
        .update(DOMAIN)
        .update(epoch.to_be_bytes())
        .update(reporter.as_bytes());
    for root in pending {
        builder = builder
            .update(root.root.as_bytes())
            .update(root.previous.as_bytes())
            .update(root.epoch.to_be_bytes());
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};

    #[test]
    fn close_report_roundtrip_and_signature() {
        let report = CloseReport::new(
            7,
            [QualifiedRoot::new_test_instance().with_epoch(7)],
            &PrivateKey::from(1),
        );
        assert!(report.validate());
        assert_deserializable(&Message::CloseReport(report));
    }
}
