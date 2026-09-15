use crate::MessageVariant;
use bitvec::prelude::BitArray;
use rsnano_types::{
    Blake2HashBuilder, BlockHash, DeserializationError, PrivateKey, PublicKey, Signature,
};
use serde::{Deserialize, Serialize};

/// A digest-only close statement or a bounded delta against a reconstructed base.
/// Vote kinds 0..=4 carry only a digest; 5 answers a delta recovery request,
/// 6 acknowledges a persisted close, and 7 requests one page on a mismatch.
/// Kind 8 announces a drained replica's membership digest together with one
/// digest per member bucket, and 9 carries the members of one bucket to a
/// replica whose bucket digest differed. Neither enters a vote tally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochClose {
    pub epoch: u64,
    pub round: u64,
    pub parent: BlockHash,
    /// Identity of the previous finalized epoch close, separate from the round parent.
    pub previous_close: BlockHash,
    pub state: BlockHash,
    pub kind: u8,
    pub voter: PublicKey,
    pub signature: Signature,
    pub page: u16,
    pub pages: u16,
    /// Empty for votes; base digests in requests; additions in delta responses.
    pub hashes: Vec<BlockHash>,
    #[serde(default, skip_serializing_if = "BlockHash::is_zero")]
    pub base: BlockHash,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<BlockHash>,
    /// Proposer's epoch member count. Replicas start a close round only once
    /// their own membership has caught up with the most advanced proposal.
    #[serde(default)]
    pub members: u64,
}
impl EpochClose {
    pub const PAGE_SIZE: usize = 512;
    pub const MAX_PAGES: u16 = 2048;
    /// Snapshot digests a recovery request may advertise as candidate bases.
    pub const MAX_BASES: usize = 8;
    /// Members are bucketed by their first byte for readiness reconciliation.
    pub const BUCKETS: usize = 256;
    pub fn bucket_of(hash: &BlockHash) -> usize {
        hash.as_bytes()[0] as usize
    }
    pub fn candidate_id(&self) -> BlockHash {
        Blake2HashBuilder::new()
            .update(b"rai-close-candidate-v2")
            .update(self.epoch.to_le_bytes())
            .update(self.round.to_le_bytes())
            .update(self.parent.as_bytes())
            .update(self.previous_close.as_bytes())
            .update(self.state.as_bytes())
            .build()
    }
    pub fn signing_hash(&self) -> BlockHash {
        let builder = Blake2HashBuilder::new()
            .update(b"rai-close-vote-v2")
            .update(self.candidate_id().as_bytes())
            .update([self.kind])
            .update(self.members.to_le_bytes());
        if self.kind == 5 {
            let mut builder = builder
                .update(self.base.as_bytes())
                .update(self.page.to_le_bytes())
                .update(self.pages.to_le_bytes());
            for hash in &self.hashes {
                builder = builder.update(hash.as_bytes());
            }
            builder.build()
        } else if matches!(self.kind, 8 | 9) {
            let mut builder = builder
                .update(self.page.to_le_bytes())
                .update(self.pages.to_le_bytes());
            for hash in &self.hashes {
                builder = builder.update(hash.as_bytes());
            }
            builder.build()
        } else if self.kind == 7 {
            let mut builder = builder.update(self.page.to_le_bytes());
            for base in &self.hashes {
                builder = builder.update(base.as_bytes());
            }
            builder.build()
        } else {
            builder.build()
        }
    }
    pub fn sign(&mut self, key: &PrivateKey) {
        self.voter = key.public_key();
        self.signature = key.sign(self.signing_hash().as_bytes());
    }
    pub fn valid_vote(&self) -> bool {
        self.kind <= 4
            && self.hashes.is_empty()
            && self.base.is_zero()
            && self.removed.is_empty()
            && self.pages == 0
            && self.page == 0
            && (!matches!(self.kind, 3 | 4) || (self.parent.is_zero() && self.state.is_zero()))
            && self
                .voter
                .verify(self.signing_hash().as_bytes(), &self.signature)
                .is_ok()
    }
    /// Receipt only: never usable as a consensus vote or certificate.
    pub fn valid_receipt(&self) -> bool {
        self.kind == 6
            && self.base.is_zero()
            && self.removed.is_empty()
            && self.round == 0
            && self.parent.is_zero()
            && self.hashes.is_empty()
            && self.pages == 0
            && self.page == 0
            && self
                .voter
                .verify(self.signing_hash().as_bytes(), &self.signature)
                .is_ok()
    }

    /// Readiness announcement: the digest and member count of the announcer's
    /// snapshot plus one digest per bucket, so a peer can name the buckets that
    /// differ without either side sending its whole membership.
    pub fn valid_announcement(&self) -> bool {
        self.kind == 8
            && self.hashes.len() == Self::BUCKETS
            && self.base.is_zero()
            && self.removed.is_empty()
            && self.parent.is_zero()
            && self.page == 0
            && self.pages == Self::BUCKETS as u16
            && self
                .voter
                .verify(self.signing_hash().as_bytes(), &self.signature)
                .is_ok()
    }

    /// One bucket of the sender's membership, sent to a replica whose digest for
    /// that bucket differed. `round` numbers the chunks of an oversized bucket.
    pub fn valid_bucket_page(&self) -> bool {
        self.kind == 9
            && self.pages == Self::BUCKETS as u16
            && (self.page as usize) < Self::BUCKETS
            && self.hashes.len() <= Self::PAGE_SIZE
            && self.base.is_zero()
            && self.removed.is_empty()
            && self.parent.is_zero()
            && self.hashes.windows(2).all(|w| w[0] < w[1])
            && self
                .hashes
                .iter()
                .all(|hash| Self::bucket_of(hash) == self.page as usize)
            && self
                .voter
                .verify(self.signing_hash().as_bytes(), &self.signature)
                .is_ok()
    }

    pub fn valid_recovery_request(&self) -> bool {
        self.kind == 7
            && !self.hashes.is_empty()
            && self.hashes.len() <= Self::MAX_BASES
            && self.base.is_zero()
            && self.removed.is_empty()
            && self.pages == 0
            && self.page < Self::MAX_PAGES
            && self
                .voter
                .verify(self.signing_hash().as_bytes(), &self.signature)
                .is_ok()
    }

    pub fn valid_delta(&self) -> bool {
        self.kind == 5
            && self.removed.is_empty()
            && !self.base.is_zero()
            && self.pages > 0
            && self.pages <= Self::MAX_PAGES
            && self.page < self.pages
            && self.hashes.len() <= Self::PAGE_SIZE
            && self
                .voter
                .verify(self.signing_hash().as_bytes(), &self.signature)
                .is_ok()
    }

    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        serde_json::to_writer(writer, self).map_err(std::io::Error::other)
    }
    pub fn deserialize(payload: &[u8]) -> Result<Self, DeserializationError> {
        let value: Self =
            serde_json::from_slice(payload).map_err(|_| DeserializationError::InvalidData)?;
        if value.kind > 9
            || !value.removed.is_empty()
            || value.hashes.len() > Self::PAGE_SIZE
            || value.pages > Self::MAX_PAGES
        {
            return Err(DeserializationError::InvalidData);
        }
        Ok(value)
    }
}
impl MessageVariant for EpochClose {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_request_signature_binds_bases_and_page() {
        let mut request = EpochClose {
            epoch: 0,
            round: 1,
            previous_close: BlockHash::ZERO,
            parent: 2.into(),
            state: 3.into(),
            kind: 7,
            voter: 0.into(),
            signature: Signature::new(),
            page: 0,
            pages: 0,
            hashes: vec![4.into()],
            base: BlockHash::ZERO,
            removed: vec![],
            members: 0,
        };
        request.sign(&PrivateKey::from(1));
        assert!(request.valid_recovery_request());
        assert!(!request.valid_vote());
        crate::assert_deserializable(&crate::Message::EpochClose(request.clone()));
        let mut changed = request.clone();
        changed.page = 1;
        assert!(!changed.valid_recovery_request());
        changed = request.clone();
        changed.hashes[0] = 6.into();
        assert!(!changed.valid_recovery_request());
        request.hashes = (1..=EpochClose::MAX_BASES as u64).map(Into::into).collect();
        request.sign(&PrivateKey::from(1));
        assert!(
            request.valid_recovery_request(),
            "a snapshot history is a valid base set"
        );
        request.hashes.push(99.into());
        request.sign(&PrivateKey::from(1));
        assert!(!request.valid_recovery_request());
        request.hashes.clear();
        request.sign(&PrivateKey::from(1));
        assert!(
            !request.valid_recovery_request(),
            "an empty-base request cannot request a full list"
        );
    }
    #[test]
    fn announcement_carries_one_digest_per_bucket_and_never_votes() {
        let mut announcement = EpochClose {
            epoch: 0,
            round: 3,
            previous_close: BlockHash::ZERO,
            parent: BlockHash::ZERO,
            state: 3.into(),
            kind: 8,
            voter: 0.into(),
            signature: Signature::new(),
            page: 0,
            pages: EpochClose::BUCKETS as u16,
            hashes: (0..EpochClose::BUCKETS as u64).map(Into::into).collect(),
            base: BlockHash::ZERO,
            removed: vec![],
            members: 7,
        };
        announcement.sign(&PrivateKey::from(1));
        assert!(announcement.valid_announcement());
        assert!(!announcement.valid_vote());
        assert!(!announcement.valid_bucket_page());
        crate::assert_deserializable(&crate::Message::EpochClose(announcement.clone()));
        let mut changed = announcement.clone();
        changed.hashes[5] = 99.into();
        assert!(!changed.valid_announcement(), "bucket digests are signed");
        changed = announcement.clone();
        changed.hashes.pop();
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_announcement());
        changed = announcement.clone();
        changed.parent = 1.into();
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_announcement());
    }

    #[test]
    fn bucket_page_holds_sorted_members_of_its_bucket() {
        let mut member = [0u8; 32];
        member[0] = 9;
        member[31] = 1;
        let mut other = member;
        other[31] = 2;
        let mut page = EpochClose {
            epoch: 0,
            round: 0,
            previous_close: BlockHash::ZERO,
            parent: BlockHash::ZERO,
            state: 3.into(),
            kind: 9,
            voter: 0.into(),
            signature: Signature::new(),
            page: 9,
            pages: EpochClose::BUCKETS as u16,
            hashes: vec![BlockHash::from_bytes(member), BlockHash::from_bytes(other)],
            base: BlockHash::ZERO,
            removed: vec![],
            members: 2,
        };
        page.sign(&PrivateKey::from(1));
        assert!(page.valid_bucket_page());
        assert!(!page.valid_vote());
        assert!(!page.valid_announcement());
        crate::assert_deserializable(&crate::Message::EpochClose(page.clone()));
        let mut changed = page.clone();
        changed.round = 1;
        assert!(!changed.valid_bucket_page(), "the chunk index is signed");
        changed = page.clone();
        changed.page = 8;
        changed.sign(&PrivateKey::from(1));
        assert!(
            !changed.valid_bucket_page(),
            "members must belong to the bucket"
        );
        changed = page.clone();
        changed.hashes.reverse();
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_bucket_page(), "members must be sorted");
        changed = page.clone();
        changed.hashes.clear();
        changed.sign(&PrivateKey::from(1));
        assert!(
            changed.valid_bucket_page(),
            "an empty bucket is a valid page"
        );
    }

    #[test]
    fn close_receipt_cannot_be_used_as_a_vote() {
        let mut receipt = EpochClose {
            epoch: 1,
            round: 0,
            previous_close: BlockHash::ZERO,
            parent: BlockHash::ZERO,
            state: 9.into(),
            kind: 6,
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            page: 0,
            pages: 0,
            hashes: vec![],
            base: BlockHash::ZERO,
            removed: vec![],
            members: 0,
        };
        receipt.sign(&PrivateKey::from(1));
        assert!(receipt.valid_receipt());
        assert!(!receipt.valid_vote());
        crate::assert_deserializable(&crate::Message::EpochClose(receipt.clone()));
        receipt.state = 10.into();
        assert!(!receipt.valid_receipt());
    }

    #[test]
    fn close_signature_binds_epoch_round_parent_state_and_kind() {
        let mut v = EpochClose {
            epoch: 3,
            round: 7,
            previous_close: BlockHash::ZERO,
            parent: 8.into(),
            state: 9.into(),
            kind: 0,
            voter: PublicKey::ZERO,
            signature: Signature::new(),
            page: 0,
            pages: 0,
            hashes: vec![],
            base: BlockHash::ZERO,
            removed: vec![],
            members: 0,
        };
        v.sign(&PrivateKey::from(1));
        assert!(v.valid_vote());
        for field in 0..6 {
            let mut changed = v.clone();
            match field {
                0 => changed.epoch += 1,
                1 => changed.round += 1,
                2 => changed.parent = 10.into(),
                3 => changed.state = 10.into(),
                4 => changed.members += 1,
                _ => changed.kind = 1,
            }
            assert!(!changed.valid_vote());
        }
        let message = crate::Message::EpochClose(v);
        crate::assert_deserializable(&message);
    }
}
