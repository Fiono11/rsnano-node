use rsnano_network::ChannelId;
use rsnano_node::representatives::QuorumSnapshot;
use rsnano_types::{Account, Amount};

#[derive(Default)]
pub(crate) struct RepresentativesViewModel {
    pub quorum: QuorumSnapshot,
    pub reps: Vec<RepresentativeViewModel>,
}

pub(crate) struct RepresentativeViewModel {
    pub account: Account,
    pub name: &'static str,
    pub weight: Amount,
    pub channel: Option<ChannelId>,
}
