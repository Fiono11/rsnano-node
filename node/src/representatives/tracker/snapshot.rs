use crate::representatives::tracker::registry::RepresentativeRegistry;
use rsnano_ledger::RepWeights;
use rsnano_network::ChannelId;
use rsnano_types::{Amount, PublicKey};

#[derive(Clone)]
pub struct RegisteredRepSnapshot {
    pub rep_key: PublicKey,
    pub weight: Amount,
    pub channel: Option<ChannelId>,
}

pub(crate) struct RepRegistrySnapshot<'a> {
    registry: &'a RepresentativeRegistry,
    weights: &'a RepWeights,
}

impl<'a> RepRegistrySnapshot<'a> {
    pub(crate) fn new(registry: &'a RepresentativeRegistry, weights: &'a RepWeights) -> Self {
        Self { registry, weights }
    }
}
