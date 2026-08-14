use super::{ConfirmReq, MessageVariant};
use bitvec::prelude::BitArray;
use rsnano_types::{DeserializationError, Vote};
use std::fmt::{Debug, Display};
/*
 * Binary Format:
 * [message_header] Common message header
 * [variable] Vote
 * - Serialized/deserialized by the `nano::vote` class.
 *
 * Header extensions:
 * - [0xf000] Count (for V1 protocol)
 * - [0x0f00] Block type
 *   - Not used anymore (V25.1+), but still present and set to `not_a_block = 0x1` for backwards compatibility
 * - [0xf000 (high), 0x00f0 (low)] Count V2 masks (for V2 protocol)
 * - [0x0001] Confirm V2 flag
 * - [0x0002] Reserved for V3+ versioning
 * - [0x0004] Rebroadcasted flag
 */
#[derive(Clone, Debug)]
pub struct ConfirmAck {
    vote: Vote,
    is_rebroadcasted: bool,
    /// Messages deserialized from network should have their digest set
    pub digest: u128,
}

impl ConfirmAck {
    pub const HASHES_MAX: usize = 255;
    #[cfg(feature = "rai_protocol")]
    pub const RAI_COMPACT_SLOT_FLAG: usize = 1;
    pub const REBROADCASTED_FLAG: usize = 2;
    #[cfg(feature = "rai_protocol")]
    pub const RAI_TIMEOUT_SLOT_FLAG: usize = 3;

    pub fn new_with_own_vote(vote: Vote) -> Self {
        assert!(vote.hashes.len() <= Self::HASHES_MAX);
        Self {
            vote,
            is_rebroadcasted: false,
            digest: 0,
        }
    }

    pub fn new_with_rebroadcasted_vote(vote: Vote) -> Self {
        assert!(vote.hashes.len() <= Self::HASHES_MAX);
        Self {
            vote,
            is_rebroadcasted: true,
            digest: 0,
        }
    }

    pub fn new_test_instance() -> Self {
        Self::new_with_own_vote(Vote::new_test_instance())
    }

    pub fn vote(&self) -> &Vote {
        &self.vote
    }

    pub fn is_rebroadcasted(&self) -> bool {
        self.is_rebroadcasted
    }

    pub fn serialized_size(extensions: BitArray<u16>) -> usize {
        let count = ConfirmReq::count(extensions);
        #[cfg(feature = "rai_protocol")]
        if extensions[Self::RAI_COMPACT_SLOT_FLAG] {
            return Vote::serialized_size_rai_slot(count as usize);
        }
        #[cfg(feature = "rai_protocol")]
        if extensions[Self::RAI_TIMEOUT_SLOT_FLAG] {
            return Vote::serialized_size_rai_timeout_slot(count as usize);
        }
        Vote::serialized_size(count as usize)
    }

    pub fn deserialize(
        bytes: &[u8],
        extensions: BitArray<u16>,
        digest: u128,
    ) -> Result<Self, DeserializationError> {
        #[cfg(feature = "rai_protocol")]
        let vote =
            if extensions[Self::RAI_COMPACT_SLOT_FLAG] && extensions[Self::RAI_TIMEOUT_SLOT_FLAG] {
                return Err(DeserializationError::InvalidData);
            } else if extensions[Self::RAI_COMPACT_SLOT_FLAG] {
                Vote::deserialize_rai_slot(bytes)?
            } else if extensions[Self::RAI_TIMEOUT_SLOT_FLAG] {
                Vote::deserialize_rai_timeout_slot(bytes)?
            } else {
                Vote::deserialize(bytes)?
            };
        #[cfg(not(feature = "rai_protocol"))]
        let vote = Vote::deserialize(bytes)?;
        if vote.hashes.len() != ConfirmReq::count(extensions) as usize {
            return Err(DeserializationError::InvalidData);
        }

        let is_rebroadcasted = extensions[Self::REBROADCASTED_FLAG];
        let mut ack = if is_rebroadcasted {
            ConfirmAck::new_with_rebroadcasted_vote(vote)
        } else {
            ConfirmAck::new_with_own_vote(vote)
        };
        ack.digest = digest;

        Ok(ack)
    }

    pub fn serialize<T>(&self, writer: &mut T) -> std::io::Result<()>
    where
        T: std::io::Write,
    {
        self.vote.serialize(writer)
    }
}

impl MessageVariant for ConfirmAck {
    fn header_extensions(&self, _payload_len: u16) -> BitArray<u16> {
        let mut extensions = BitArray::default();
        extensions |= ConfirmReq::count_bits(self.vote.hashes.len() as u8);
        #[cfg(feature = "rai_protocol")]
        extensions.set(Self::RAI_COMPACT_SLOT_FLAG, self.vote.is_rai_slot_batch());
        #[cfg(feature = "rai_protocol")]
        extensions.set(
            Self::RAI_TIMEOUT_SLOT_FLAG,
            self.vote.is_rai_timeout_slot_batch(),
        );
        extensions.set(Self::REBROADCASTED_FLAG, self.is_rebroadcasted);
        extensions
    }
}

impl PartialEq for ConfirmAck {
    fn eq(&self, other: &Self) -> bool {
        self.vote == other.vote
    }
}

impl Eq for ConfirmAck {}

impl Display for ConfirmAck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.vote, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, assert_deserializable};
    use rsnano_types::{BlockHash, PrivateKey, UnixMillisTimestamp};

    #[test]
    fn serialize_v1() {
        let keys = PrivateKey::new();
        let hashes = vec![BlockHash::from(1)];
        let vote = Vote::new(&keys, UnixMillisTimestamp::ZERO, 0, hashes);
        let confirm = Message::ConfirmAck(ConfirmAck::new_with_own_vote(vote));

        assert_deserializable(&confirm);
    }

    #[test]
    fn serialize_v2() {
        let keys = PrivateKey::new();
        let mut hashes = Vec::new();
        for i in 0..ConfirmAck::HASHES_MAX {
            hashes.push(BlockHash::from(i as u64))
        }
        let vote = Vote::new(&keys, UnixMillisTimestamp::ZERO, 0, hashes);
        let confirm = Message::ConfirmAck(ConfirmAck::new_with_own_vote(vote));

        assert_deserializable(&confirm);
    }

    #[test]
    #[should_panic]
    fn panics_when_vote_contains_too_many_hashes() {
        let keys = PrivateKey::new();
        let hashes = vec![BlockHash::from(1); 256];
        let vote = Vote::new(&keys, UnixMillisTimestamp::ZERO, 0, hashes);
        Message::ConfirmAck(ConfirmAck::new_with_own_vote(vote));
    }

    #[test]
    fn rebroadcasted_vote() {
        let ack = ConfirmAck::new_with_rebroadcasted_vote(Vote::new_test_instance());
        assert_eq!(ack.is_rebroadcasted(), true);
    }

    #[test]
    fn extensions_without_rebroadcasted_flag() {
        let ack = ConfirmAck::new_with_own_vote(Vote::new_test_instance());
        let extensions = ack.header_extensions(0);
        assert_eq!(extensions[ConfirmAck::REBROADCASTED_FLAG], false);
    }

    #[test]
    fn extensions_with_rebroadcasted_flag() {
        let ack = ConfirmAck::new_with_rebroadcasted_vote(Vote::new_test_instance());
        let extensions = ack.header_extensions(0);
        assert_eq!(extensions[ConfirmAck::REBROADCASTED_FLAG], true);
    }

    #[test]
    fn deserialize_set_rebroadcasted_flag() {
        let mut bytes = Vec::new();
        let vote = Vote::new_test_instance();
        vote.serialize(&mut bytes).unwrap();

        let mut extensions = ConfirmAck::new_with_own_vote(vote.clone()).header_extensions(0);
        extensions.set(ConfirmAck::REBROADCASTED_FLAG, true);

        let ack = ConfirmAck::deserialize(&bytes, extensions, 0).unwrap();
        assert_eq!(ack.is_rebroadcasted(), true);
    }

    #[test]
    fn deserialize_unset_rebroadcasted_flag() {
        let mut bytes = Vec::new();
        let vote = Vote::new_test_instance();
        vote.serialize(&mut bytes).unwrap();

        let mut extensions = ConfirmAck::new_with_own_vote(vote.clone()).header_extensions(0);
        extensions.set(ConfirmAck::REBROADCASTED_FLAG, false);

        let ack = ConfirmAck::deserialize(&bytes, extensions, 0).unwrap();

        assert_eq!(ack.is_rebroadcasted(), false);
    }

    #[test]
    fn deserialize_rejects_header_count_mismatch() {
        let vote = Vote::new(
            &PrivateKey::from(42),
            UnixMillisTimestamp::ZERO,
            0,
            vec![BlockHash::from(1), BlockHash::from(2)],
        );
        let mut bytes = Vec::new();
        vote.serialize(&mut bytes).unwrap();

        let extensions = ConfirmReq::count_bits(1);
        assert!(ConfirmAck::deserialize(&bytes, extensions, 0).is_err());
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_batch_uses_existing_confirm_ack_count_header() {
        use rsnano_types::{
            RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId, RaiVoteMetadata, RaiVotePhase,
        };

        let metadata = RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::new(7),
                ..Default::default()
            }),
            phase: RaiVotePhase::First,
            epoch: RaiEpoch::new(7),
            scope: RaiCommitteeScope::All,
        };
        let vote = Vote::new_rai_batch(
            &PrivateKey::from(42),
            UnixMillisTimestamp::ZERO,
            0,
            [
                (metadata.clone(), BlockHash::from(1)),
                (metadata, BlockHash::from(2)),
            ],
        );
        let ack = ConfirmAck::new_with_own_vote(vote);
        let extensions = ack.header_extensions(0);

        assert_eq!(ConfirmReq::count(extensions), 2);
        assert_deserializable(&Message::ConfirmAck(ack));
    }

    #[cfg(feature = "rai_protocol")]
    #[test]
    fn rai_timeout_batch_round_trips_account_height_locator() {
        use rsnano_types::{
            Account, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId, RaiTimeoutSlot,
            RaiVoteMetadata, RaiVotePhase,
        };

        let vote = Vote::new_rai_timeout_batch(
            &PrivateKey::from(42),
            UnixMillisTimestamp::ZERO,
            0,
            [(
                RaiVoteMetadata {
                    election_id: RaiElectionId::Slot(RaiSlotId {
                        epoch: RaiEpoch::new(7),
                        ..Default::default()
                    }),
                    phase: RaiVotePhase::Notar,
                    epoch: RaiEpoch::new(7),
                    scope: RaiCommitteeScope::All,
                },
                RaiTimeoutSlot {
                    account: Account::from(9),
                    height: 11,
                },
            )],
        );
        let ack = ConfirmAck::new_with_own_vote(vote);
        let extensions = ack.header_extensions(0);

        assert!(extensions[ConfirmAck::RAI_TIMEOUT_SLOT_FLAG]);
        assert!(!extensions[ConfirmAck::RAI_COMPACT_SLOT_FLAG]);
        assert_deserializable(&Message::ConfirmAck(ack));
    }
}
