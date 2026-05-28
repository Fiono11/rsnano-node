use std::sync::Arc;

use crate::{Channel, ChannelId};

/// Emitted by `Network` when a channel is established (handshake complete)
/// or removed (closed and purged).
#[derive(Clone)]
pub enum ChannelEvent {
    Established(Arc<Channel>),
    Removed(ChannelId),
}
