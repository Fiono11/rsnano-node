use rsnano_network::{Channel, ChannelId};
use rsnano_types::{BlockHash, Vote, VoteDelivery};
use std::{ops::Deref, sync::Arc};

#[derive(Clone)]
pub struct ReceivedVote {
    pub vote: Arc<Vote>,
    pub delivery: VoteDelivery,
    pub channel: Option<Arc<Channel>>,
}

impl ReceivedVote {
    pub fn new(vote: Arc<Vote>, source: VoteDelivery, channel: Option<Arc<Channel>>) -> Self {
        Self {
            vote,
            delivery: source,
            channel,
        }
    }

    pub fn channel_id(&self) -> Option<ChannelId> {
        self.channel.as_ref().map(|c| c.channel_id())
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
        self.vote.hashes.iter().filter(|&h| {
            if self.filter.is_zero() {
                true
            } else {
                *h == self.filter
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
