use rsnano_network::ChannelId;
use rsnano_types::PublicKey;

/// A representative to which we have a direct connection
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeeredRep {
    pub public_key: PublicKey,
    pub channel_id: ChannelId,
}

impl PeeredRep {
    pub fn new(public_key: PublicKey, channel_id: ChannelId) -> Self {
        Self {
            public_key,
            channel_id,
        }
    }
}
