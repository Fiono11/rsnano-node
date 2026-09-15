use crate::MessageVariant;
use bitvec::prelude::BitArray;
use rsnano_types::{
    Blake2HashBuilder, BlockHash, DeserializationError, PrivateKey, PublicKey, Signature,
};
use serde::{Deserialize, Serialize};

/// A digest-only close statement or one page of a membership digest tree.
/// Vote kinds 0..=4 carry only a digest and 6 acknowledges a persisted close.
/// Kind 8 announces a drained replica's membership: its tree root, member
/// count and a set sketch from which a peer decodes the differing members in
/// one step. Kind 9 requests one page of a view named by its root, answered by
/// kind 5 (the 256 level-1 digests, or one bucket's leaf digests) or kind 7
/// (the members of one two-byte prefix); pages are the fallback when a sketch
/// does not decode. Pages never enter a vote tally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochClose {
    pub epoch: u64,
    /// Close round for votes; announcement sequence for kind 8; chunk index for kind 7.
    pub round: u64,
    pub parent: BlockHash,
    /// Identity of the previous finalized epoch close, separate from the round parent.
    pub previous_close: BlockHash,
    /// Membership tree root: the proposed close for votes, the announced or
    /// requested view for pages.
    pub state: BlockHash,
    pub kind: u8,
    pub voter: PublicKey,
    pub signature: Signature,
    /// Level-1 bucket (kind 5, 9 at level 1) or two-byte prefix (kind 7, 9 at level 2).
    pub page: u16,
    /// Tree level of a page or request: 1 for a level-1 bucket, 2 for a leaf.
    pub pages: u16,
    /// Empty for votes and requests; level-1 digests, level-2 entries or members in pages.
    pub hashes: Vec<BlockHash>,
    #[serde(default, skip_serializing_if = "BlockHash::is_zero")]
    pub base: BlockHash,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<BlockHash>,
    /// Member count of the proposer's or announcer's membership.
    #[serde(default)]
    pub members: u64,
    /// Hex-encoded membership sketch of an announcement, empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sketch: String,
}
impl EpochClose {
    pub const PAGE_SIZE: usize = 512;
    /// Members are bucketed by their first byte at level 1 of the digest tree.
    pub const LEVEL1_BUCKETS: usize = 256;
    pub const LEVEL2_PAGE: u16 = 1;
    pub const LEAF_PAGE: u16 = 2;
    pub const LEVEL1_PAGE: u16 = 3;
    /// Serialized size of a membership sketch (192 cells of 44 bytes).
    pub const SKETCH_BYTES: usize = 192 * 44;
    pub fn sketch_bytes(&self) -> Option<Vec<u8>> {
        decode_hex(&self.sketch)
    }
    pub fn set_sketch(&mut self, bytes: &[u8]) {
        self.sketch = encode_hex(bytes);
    }
    pub fn prefix_of(hash: &BlockHash) -> u16 {
        u16::from_be_bytes([hash.as_bytes()[0], hash.as_bytes()[1]])
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
        if matches!(self.kind, 5 | 7 | 8 | 9) {
            let mut builder = builder
                .update(self.page.to_le_bytes())
                .update(self.pages.to_le_bytes())
                .update(self.sketch.as_bytes());
            for hash in &self.hashes {
                builder = builder.update(hash.as_bytes());
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
    fn signature_valid(&self) -> bool {
        self.voter
            .verify(self.signing_hash().as_bytes(), &self.signature)
            .is_ok()
    }
    fn page_fields_clear(&self) -> bool {
        self.base.is_zero()
            && self.removed.is_empty()
            && self.parent.is_zero()
            && self.sketch.is_empty()
    }
    pub fn valid_vote(&self) -> bool {
        self.kind <= 4
            && self.hashes.is_empty()
            && self.sketch.is_empty()
            && self.base.is_zero()
            && self.removed.is_empty()
            && self.pages == 0
            && self.page == 0
            && (!matches!(self.kind, 3 | 4) || (self.parent.is_zero() && self.state.is_zero()))
            && self.signature_valid()
    }
    /// Receipt only: never usable as a consensus vote or certificate.
    pub fn valid_receipt(&self) -> bool {
        self.kind == 6
            && self.round == 0
            && self.hashes.is_empty()
            && self.pages == 0
            && self.page == 0
            && self.page_fields_clear()
            && self.signature_valid()
    }
    /// Readiness announcement: the root and member count of the announcer's
    /// membership plus its sketch, from which a peer decodes the members on
    /// either side only without either side sending its whole membership.
    pub fn valid_announcement(&self) -> bool {
        self.kind == 8
            && self.hashes.is_empty()
            && self.sketch.len() == Self::SKETCH_BYTES * 2
            && self.sketch_bytes().is_some()
            && self.page == 0
            && self.pages == 0
            && self.base.is_zero()
            && self.removed.is_empty()
            && self.parent.is_zero()
            && self.signature_valid()
    }
    /// Request for one page of the view whose root is `state`.
    pub fn valid_view_request(&self) -> bool {
        self.kind == 9
            && self.hashes.is_empty()
            && (self.pages == Self::LEVEL2_PAGE && (self.page as usize) < Self::LEVEL1_BUCKETS
                || self.pages == Self::LEAF_PAGE
                || self.pages == Self::LEVEL1_PAGE && self.page == 0)
            && self.page_fields_clear()
            && self.signature_valid()
    }
    /// The 256 level-1 digests of a view, for a peer whose sketch did not decode.
    pub fn valid_level1_page(&self) -> bool {
        self.kind == 5
            && self.pages == Self::LEVEL1_PAGE
            && self.page == 0
            && self.hashes.len() == Self::LEVEL1_BUCKETS
            && self.page_fields_clear()
            && self.signature_valid()
    }
    /// One level-1 bucket of a view: one entry per non-empty leaf, the
    /// sub-bucket index in the first byte, in ascending order.
    pub fn valid_level2_page(&self) -> bool {
        self.kind == 5
            && self.pages == Self::LEVEL2_PAGE
            && (self.page as usize) < Self::LEVEL1_BUCKETS
            && self.hashes.len() <= Self::LEVEL1_BUCKETS
            && self
                .hashes
                .windows(2)
                .all(|w| w[0].as_bytes()[0] < w[1].as_bytes()[0])
            && self.page_fields_clear()
            && self.signature_valid()
    }
    /// The members of one two-byte prefix of a view, sorted; `round` numbers
    /// the chunks of an oversized leaf.
    pub fn valid_leaf_page(&self) -> bool {
        self.kind == 7
            && self.pages == Self::LEAF_PAGE
            && self.hashes.len() <= Self::PAGE_SIZE
            && self.hashes.windows(2).all(|w| w[0] < w[1])
            && self
                .hashes
                .iter()
                .all(|hash| Self::prefix_of(hash) == self.page)
            && self.page_fields_clear()
            && self.signature_valid()
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
            || value.sketch.len() > Self::SKETCH_BYTES * 2
        {
            return Err(DeserializationError::InvalidData);
        }
        Ok(value)
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
            Some(digit(pair[0])? << 4 | digit(pair[1])?)
        })
        .collect()
}
impl MessageVariant for EpochClose {
    fn header_extensions(&self, payload_len: u16) -> BitArray<u16> {
        BitArray::new(payload_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(kind: u8) -> EpochClose {
        EpochClose {
            epoch: 0,
            round: 0,
            previous_close: BlockHash::ZERO,
            parent: BlockHash::ZERO,
            state: 3.into(),
            kind,
            voter: 0.into(),
            signature: Signature::new(),
            page: 0,
            pages: 0,
            hashes: vec![],
            base: BlockHash::ZERO,
            removed: vec![],
            members: 0,
            sketch: String::new(),
        }
    }

    fn with_prefix(first: u8, second: u8, tail: u8) -> BlockHash {
        let mut bytes = [0u8; 32];
        bytes[0] = first;
        bytes[1] = second;
        bytes[31] = tail;
        BlockHash::from_bytes(bytes)
    }

    #[test]
    fn announcement_carries_a_sketch_and_never_votes() {
        let mut announcement = packet(8);
        announcement.round = 3;
        announcement.members = 7;
        let bytes: Vec<u8> = (0..EpochClose::SKETCH_BYTES).map(|i| i as u8).collect();
        announcement.set_sketch(&bytes);
        announcement.sign(&PrivateKey::from(1));
        assert!(announcement.valid_announcement());
        assert_eq!(announcement.sketch_bytes(), Some(bytes.clone()));
        assert!(!announcement.valid_vote());
        assert!(!announcement.valid_level2_page());
        crate::assert_deserializable(&crate::Message::EpochClose(announcement.clone()));
        let mut changed = announcement.clone();
        changed.sketch.replace_range(0..2, "ff");
        assert!(!changed.valid_announcement(), "the sketch is signed");
        changed = announcement.clone();
        changed.round = 4;
        assert!(!changed.valid_announcement(), "the sequence is signed");
        changed = announcement.clone();
        changed.set_sketch(&bytes[1..]);
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_announcement(), "the sketch has a fixed size");
        changed = announcement.clone();
        changed.sketch.replace_range(0..2, "zz");
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_announcement(), "the sketch must be hex");
        changed = announcement.clone();
        changed.parent = 1.into();
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_announcement());
        changed = announcement.clone();
        changed.hashes.push(1.into());
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_announcement(), "no digests ride along");
    }

    #[test]
    fn level1_page_lists_every_bucket_digest() {
        let mut page = packet(5);
        page.pages = EpochClose::LEVEL1_PAGE;
        page.hashes = (0..EpochClose::LEVEL1_BUCKETS as u64)
            .map(Into::into)
            .collect();
        page.sign(&PrivateKey::from(1));
        assert!(page.valid_level1_page());
        assert!(!page.valid_level2_page());
        crate::assert_deserializable(&crate::Message::EpochClose(page.clone()));
        let mut changed = page.clone();
        changed.hashes.pop();
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_level1_page());
        changed = page.clone();
        changed.hashes[5] = 99.into();
        assert!(!changed.valid_level1_page(), "digests are signed");
        let mut request = packet(9);
        request.pages = EpochClose::LEVEL1_PAGE;
        request.sign(&PrivateKey::from(1));
        assert!(request.valid_view_request());
        request.page = 1;
        request.sign(&PrivateKey::from(1));
        assert!(!request.valid_view_request(), "there is one level-1 page");
    }

    #[test]
    fn view_request_names_a_level1_bucket_or_a_prefix() {
        let mut request = packet(9);
        request.pages = EpochClose::LEVEL2_PAGE;
        request.page = 255;
        request.sign(&PrivateKey::from(1));
        assert!(request.valid_view_request());
        assert!(!request.valid_vote());
        crate::assert_deserializable(&crate::Message::EpochClose(request.clone()));
        let mut changed = request.clone();
        changed.page = 256;
        changed.sign(&PrivateKey::from(1));
        assert!(
            !changed.valid_view_request(),
            "only 256 level-1 buckets exist"
        );
        changed.pages = EpochClose::LEAF_PAGE;
        changed.page = 0xabcd;
        changed.sign(&PrivateKey::from(1));
        assert!(changed.valid_view_request());
        changed.pages = 4;
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_view_request());
        changed = request.clone();
        changed.state = 4.into();
        assert!(
            !changed.valid_view_request(),
            "the requested view is signed"
        );
    }

    #[test]
    fn level2_page_lists_leaf_digests_in_sub_bucket_order() {
        let mut page = packet(5);
        page.pages = EpochClose::LEVEL2_PAGE;
        page.page = 9;
        page.hashes = vec![with_prefix(1, 0, 1), with_prefix(7, 0, 2)];
        page.sign(&PrivateKey::from(1));
        assert!(page.valid_level2_page());
        assert!(!page.valid_leaf_page());
        crate::assert_deserializable(&crate::Message::EpochClose(page.clone()));
        let mut changed = page.clone();
        changed.hashes.reverse();
        changed.sign(&PrivateKey::from(1));
        assert!(
            !changed.valid_level2_page(),
            "entries are ordered by sub-bucket"
        );
        changed = page.clone();
        changed.hashes.push(with_prefix(7, 0, 3));
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_level2_page(), "one entry per sub-bucket");
        changed = page.clone();
        changed.hashes.clear();
        changed.sign(&PrivateKey::from(1));
        assert!(
            changed.valid_level2_page(),
            "an empty bucket is a valid page"
        );
        changed = page.clone();
        changed.hashes[0] = with_prefix(2, 0, 1);
        assert!(!changed.valid_level2_page(), "entries are signed");
    }

    #[test]
    fn leaf_page_holds_sorted_members_of_its_prefix() {
        let mut page = packet(7);
        page.pages = EpochClose::LEAF_PAGE;
        page.page = 0x0901;
        page.hashes = vec![with_prefix(9, 1, 1), with_prefix(9, 1, 2)];
        page.sign(&PrivateKey::from(1));
        assert!(page.valid_leaf_page());
        assert!(!page.valid_vote());
        assert!(!page.valid_announcement());
        crate::assert_deserializable(&crate::Message::EpochClose(page.clone()));
        let mut changed = page.clone();
        changed.round = 1;
        assert!(!changed.valid_leaf_page(), "the chunk index is signed");
        changed = page.clone();
        changed.page = 0x0902;
        changed.sign(&PrivateKey::from(1));
        assert!(
            !changed.valid_leaf_page(),
            "members must belong to the prefix"
        );
        changed = page.clone();
        changed.hashes.reverse();
        changed.sign(&PrivateKey::from(1));
        assert!(!changed.valid_leaf_page(), "members must be sorted");
        changed = page.clone();
        changed.hashes.clear();
        changed.sign(&PrivateKey::from(1));
        assert!(changed.valid_leaf_page(), "an empty leaf is a valid page");
    }

    #[test]
    fn close_receipt_cannot_be_used_as_a_vote() {
        let mut receipt = packet(6);
        receipt.epoch = 1;
        receipt.state = 9.into();
        receipt.sign(&PrivateKey::from(1));
        assert!(receipt.valid_receipt());
        assert!(!receipt.valid_vote());
        crate::assert_deserializable(&crate::Message::EpochClose(receipt.clone()));
        receipt.state = 10.into();
        assert!(!receipt.valid_receipt());
    }

    #[test]
    fn close_signature_binds_epoch_round_parent_state_and_kind() {
        let mut v = packet(0);
        v.epoch = 3;
        v.round = 7;
        v.parent = 8.into();
        v.state = 9.into();
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
