use rsnano_network::ChannelId;
use rsnano_nullable_clock::Timestamp;
use rsnano_types::PublicKey;

/// A representative to which we have a direct connection
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeeredRep {
    pub public_key: PublicKey,
    pub channel_id: ChannelId,
    pub last_request: Timestamp,
}

impl PeeredRep {
    pub fn new(public_key: PublicKey, channel_id: ChannelId, last_request: Timestamp) -> Self {
        Self {
            public_key,
            channel_id,
            last_request,
        }
    }
}
