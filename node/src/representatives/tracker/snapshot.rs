use std::ops::Deref;

use rsnano_ledger::RepWeights;
use rsnano_network::ChannelId;
use rsnano_types::Amount;

use crate::representatives::{
    QuorumSnapshot,
    tracker::registry::{RegisteredRep, RepresentativeRegistry},
};

#[derive(Clone)]
pub struct RegisteredRepSnapshot {
    pub rep: RegisteredRep,
    pub weight: Amount,
}

impl RegisteredRepSnapshot {
    pub fn new_test_instance() -> Self {
        Self {
            rep: RegisteredRep::new_test_instance(),
            weight: Amount::nano(100_000),
        }
    }
}

impl Deref for RegisteredRepSnapshot {
    type Target = RegisteredRep;

    fn deref(&self) -> &Self::Target {
        &self.rep
    }
}

pub struct RepRegistrySnapshot<'a> {
    registry: &'a RepresentativeRegistry,
    weights: &'a RepWeights,
    quorum: &'a QuorumSnapshot,
}

impl<'a> RepRegistrySnapshot<'a> {
    pub(crate) fn new(
        registry: &'a RepresentativeRegistry,
        weights: &'a RepWeights,
        quorum: &'a QuorumSnapshot,
    ) -> Self {
        Self {
            registry,
            weights,
            quorum,
        }
    }

    /// Query if a peer manages a principle representative
    pub fn is_principal_rep(&self, channel_id: ChannelId) -> bool {
        self.registry.reps_for_channel(channel_id).any(|rep| {
            let weight = self.weights.weight(&rep.public_key);
            weight >= self.quorum.minimum_principal_weight
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = RegisteredRepSnapshot> {
        self.registry.iter().map(|rep| RegisteredRepSnapshot {
            rep: rep.clone(),
            weight: self.weights.weight(&rep.public_key),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_nullable_clock::Timestamp;

    #[test]
    fn checks_channel_for_principal_rep() {
        let rep_non_pr = PublicKey::from(100);
        let rep_pr = PublicKey::from(200);

        let channel_non_pr = ChannelId::from(1000);
        let channel_pr = ChannelId::from(2000);
        let channel_unknown = ChannelId::from(3000);

        let now = Timestamp::new_test_instance();

        let quorum = QuorumSnapshot::new_test_instance();

        let weights = RepWeights::from([
            (rep_non_pr, Amount::nano(1)),
            (rep_pr, quorum.minimum_principal_weight),
        ]);

        let mut registry = RepresentativeRegistry::new();
        registry.register(rep_non_pr, Some(channel_non_pr), now);
        registry.register(rep_pr, Some(channel_pr), now);

        let snapshot = RepRegistrySnapshot::new(&registry, &weights, &quorum);

        assert_eq!(snapshot.is_principal_rep(channel_unknown), false, "unknown");
        assert_eq!(snapshot.is_principal_rep(channel_non_pr), false, "non-pr");
        assert_eq!(snapshot.is_principal_rep(channel_pr), true, "pr");
    }
}
