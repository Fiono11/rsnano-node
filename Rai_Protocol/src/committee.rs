use std::collections::{BTreeMap, BTreeSet};

use crate::error::{RaiError, Result};
use crate::types::{CommitteeId, ReplicaId, Weight};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Committee {
    pub id: CommitteeId,
    pub members: BTreeSet<ReplicaId>,
    pub weights: BTreeMap<ReplicaId, Weight>,
    /// Maximum Byzantine voting weight assumed by this committee snapshot.
    pub f: Weight,
    /// Fast-path participation slack, expressed as voting weight.
    pub p: Weight,
}

impl Committee {
    /// Equal-weight compatibility constructor.
    pub fn new(
        id: CommitteeId,
        members: impl IntoIterator<Item = ReplicaId>,
        f: usize,
        p: usize,
    ) -> Result<Self> {
        let weights = members
            .into_iter()
            .map(|replica| (replica, 1 as Weight))
            .collect::<BTreeMap<_, _>>();
        Self::weighted(id, weights, f as Weight, p as Weight)
    }

    pub fn weighted(
        id: CommitteeId,
        weights: impl IntoIterator<Item = (ReplicaId, Weight)>,
        f: Weight,
        p: Weight,
    ) -> Result<Self> {
        let mut collected = BTreeMap::new();
        for (replica, weight) in weights {
            if collected.insert(replica, weight).is_some() {
                return Err(RaiError::InvalidCommittee(format!(
                    "committee {id} contains duplicate weight entries for replica {replica}"
                )));
            }
        }
        let weights = collected;
        let members = weights.keys().copied().collect();
        let committee = Self {
            id,
            members,
            weights,
            f,
            p,
        };
        committee.validate()?;
        Ok(committee)
    }

    pub fn n(&self) -> usize {
        self.members.len()
    }

    pub fn total_weight(&self) -> Weight {
        self.weights.values().copied().sum()
    }

    pub fn weight(&self, replica: ReplicaId) -> Weight {
        self.weights.get(&replica).copied().unwrap_or(0)
    }

    pub fn weight_of_signers(&self, signers: impl IntoIterator<Item = ReplicaId>) -> Weight {
        signers
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|signer| self.weight(signer))
            .sum()
    }

    pub fn validate(&self) -> Result<()> {
        if self.members.is_empty() {
            return Err(RaiError::InvalidCommittee(format!(
                "committee {} is empty",
                self.id
            )));
        }
        if self.members != self.weights.keys().copied().collect() {
            return Err(RaiError::InvalidCommittee(format!(
                "committee {} member and weight maps disagree",
                self.id
            )));
        }
        if self.weights.values().any(|weight| *weight == 0) {
            return Err(RaiError::InvalidCommittee(format!(
                "committee {} contains a zero-weight member",
                self.id
            )));
        }
        if self.p < self.f {
            return Err(RaiError::InvalidCommittee(format!(
                "committee {} violates p >= f (p={}, f={})",
                self.id, self.p, self.f
            )));
        }
        let total_weight = self.weights.values().try_fold(0u128, |total, weight| {
            total
                .checked_add(*weight)
                .ok_or_else(|| RaiError::InvalidCommittee("committee total weight overflow".into()))
        })?;
        let minimum = self
            .f
            .checked_mul(3)
            .and_then(|value| self.p.checked_mul(2).and_then(|p| value.checked_add(p)))
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| {
                RaiError::InvalidCommittee("weighted committee bound overflow".into())
            })?;
        if total_weight < minimum {
            return Err(RaiError::InvalidCommittee(format!(
                "committee {} violates W >= 3F + 2P + 1 (W={}, minimum={minimum})",
                self.id, total_weight
            )));
        }
        Ok(())
    }

    pub fn contains(&self, replica: ReplicaId) -> bool {
        self.weight(replica) > 0
    }

    pub fn notar_threshold(&self) -> Weight {
        self.total_weight() - self.f - self.p
    }

    pub fn fast_threshold(&self) -> Weight {
        self.total_weight() - self.p
    }

    pub fn final_threshold(&self) -> Weight {
        self.total_weight() - self.f - self.p
    }

    pub fn second_look_threshold(&self) -> Weight {
        self.f + self.p + 1
    }

    pub fn visibility_threshold(&self) -> Weight {
        self.f + 1
    }

    pub fn report_threshold(&self) -> Weight {
        self.total_weight() - self.f
    }
}
