use rsnano_network::ChannelId;
#[cfg(feature = "rai_protocol")]
use rsnano_types::RaiVoteMetadata;
use rsnano_types::{BlockHash, Vote, VoteDelivery};
use std::{ops::Deref, sync::Arc};

#[derive(Clone)]
pub struct ReceivedVote {
    pub vote: Arc<Vote>,
    pub delivery: VoteDelivery,
    pub channel_id: Option<ChannelId>,
}

impl ReceivedVote {
    pub fn new(vote: Arc<Vote>, source: VoteDelivery, channel_id: Option<ChannelId>) -> Self {
        Self {
            vote,
            delivery: source,
            channel_id,
        }
    }
}

impl Deref for ReceivedVote {
    type Target = Vote;

    fn deref(&self) -> &Self::Target {
        &self.vote
    }
}

/// A vote where only one given block hash is counted
pub struct FilteredVote {
    pub vote: ReceivedVote,
    pub filter: BlockHash,
}

impl FilteredVote {
    pub fn new(vote: ReceivedVote, filter: BlockHash) -> Self {
        Self { vote, filter }
    }

    pub fn filtered_blocks(&self) -> impl Iterator<Item = &BlockHash> {
        self.vote.hashes().filter(|&h| {
            if self.filter.is_zero() {
                true
            } else {
                *h == self.filter
            }
        })
    }

    /// Returns the signed RAI leaves selected by this vote's hash filter.
    /// Metadata is positional in a RAI batch, so filtering must retain the
    /// metadata/hash pairing rather than filtering `hashes` independently.
    #[cfg(feature = "rai_protocol")]
    pub fn filtered_rai_entries(&self) -> impl Iterator<Item = (&RaiVoteMetadata, &BlockHash)> {
        self.vote.rai_entries().filter(|(_, hash)| {
            if self.filter.is_zero() {
                true
            } else {
                **hash == self.filter
            }
        })
    }
}

impl Deref for FilteredVote {
    type Target = ReceivedVote;

    fn deref(&self) -> &Self::Target {
        &self.vote
    }
}

impl From<ReceivedVote> for FilteredVote {
    fn from(value: ReceivedVote) -> Self {
        Self::new(value, BlockHash::ZERO)
    }
}

#[cfg(all(test, feature = "rai_protocol"))]
mod tests {
    use super::*;
    use rsnano_types::{
        PrivateKey, QualifiedRoot, RaiCommitteeScope, RaiElectionId, RaiEpoch, RaiSlotId,
        RaiVoteMetadata, RaiVotePhase, UnixMillisTimestamp,
    };

    #[test]
    fn hash_filter_preserves_rai_leaf_metadata_pairing() {
        let first_hash = BlockHash::from(1);
        let selected_hash = BlockHash::from(2);
        let first_metadata = RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::ZERO,
                root: QualifiedRoot::new_test_instance(),
            }),
            phase: RaiVotePhase::First,
            epoch: RaiEpoch::ZERO,
            scope: RaiCommitteeScope::All,
        };
        let selected_metadata = RaiVoteMetadata {
            election_id: RaiElectionId::Slot(RaiSlotId {
                epoch: RaiEpoch::new(1),
                root: QualifiedRoot::new_test_instance(),
            }),
            phase: RaiVotePhase::Final,
            epoch: RaiEpoch::new(1),
            scope: RaiCommitteeScope::Newer,
        };
        let vote = Arc::new(Vote::new_rai_batch(
            &PrivateKey::from(1),
            UnixMillisTimestamp::new(1),
            0,
            [
                (first_metadata, first_hash),
                (selected_metadata.clone(), selected_hash),
            ],
        ));
        let filtered = FilteredVote::new(
            ReceivedVote::new(vote, VoteDelivery::Direct, None),
            selected_hash,
        );

        let entries = filtered.filtered_rai_entries().collect::<Vec<_>>();

        assert_eq!(entries, vec![(&selected_metadata, &selected_hash)]);
    }
}
