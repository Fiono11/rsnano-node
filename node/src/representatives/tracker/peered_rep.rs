use std::sync::Arc;

use rsnano_network::{Channel, ChannelId};
use rsnano_nullable_clock::Timestamp;
use rsnano_types::PublicKey;

/// A representative to which we have a direct connection
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeeredRep {
    pub public_key: PublicKey,
    pub channel: Arc<Channel>,
    pub last_request: Timestamp,
}

impl PeeredRep {
    pub fn new(public_key: PublicKey, channel: Arc<Channel>, last_request: Timestamp) -> Self {
        Self {
            public_key,
            channel,
            last_request,
        }
    }

    pub fn channel_id(&self) -> ChannelId {
        self.channel.channel_id()
    }
}
