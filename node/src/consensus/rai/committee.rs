use std::sync::Arc;

use rsnano_ledger::RepWeightCache;
use rsnano_types::{Amount, PublicKey, RaiElectionId, RaiEpoch};

pub const RAI_PRINCIPAL_WEIGHT_DIVISOR: u128 = 1000;

/// Provides RAI committees derived from balance snapshots fixed by epoch close.
pub trait RaiCommitteeProvider: Send + Sync {
    fn genesis_committee(&self) -> RaiCommittee;
    fn committee_for_closed_epoch(&self, epoch: RaiEpoch) -> Option<RaiCommittee>;

    fn committee_at(&self, epoch: Option<RaiEpoch>) -> RaiCommittee {
        epoch
            .and_then(|epoch| self.committee_for_closed_epoch(epoch))
            .unwrap_or_else(|| self.genesis_committee())
    }

    fn committees_for(&self, election_id: &RaiElectionId) -> RaiCommitteeSet {
        let epoch = election_epoch(election_id);
        RaiCommitteeSet::new([
            self.committee_at(epoch.checked_sub(3)),
            self.committee_at(epoch.checked_sub(2)),
        ])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCommittee {
    members: Vec<RaiCommitteeMember>,
    thresholds: RaiCommitteeThresholds,
}

impl RaiCommittee {
    pub fn members(&self) -> &[RaiCommitteeMember] {
        &self.members
    }

    pub fn thresholds(&self) -> RaiCommitteeThresholds {
        self.thresholds
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn contains(&self, account: &PublicKey) -> bool {
        self.members
            .binary_search_by_key(account, |member| member.account)
            .is_ok()
    }

    pub fn has_final_quorum(&self, votes: usize) -> bool {
        !self.is_empty() && votes >= self.thresholds.finalization
    }

    pub fn has_fast_quorum(&self, votes: usize) -> bool {
        !self.is_empty() && votes >= self.thresholds.fast
    }

    pub fn has_notarization_quorum(&self, votes: usize) -> bool {
        !self.is_empty() && votes >= self.thresholds.notarization
    }

    pub fn has_visibility_quorum(&self, votes: usize) -> bool {
        !self.is_empty() && votes >= self.thresholds.max_faulty + 1
    }

    fn has_same_members_as(&self, other: &Self) -> bool {
        self.members
            .iter()
            .map(|member| member.account)
            .eq(other.members.iter().map(|member| member.account))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaiCommitteeMember {
    pub account: PublicKey,
    pub balance: Amount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaiCommitteeThresholds {
    pub size: usize,
    pub max_faulty: usize,
    pub max_offline: usize,
    pub notarization: usize,
    pub fast: usize,
    pub finalization: usize,
}

impl RaiCommitteeThresholds {
    pub fn for_size(size: usize) -> Self {
        Self::with_faults(size, size.saturating_sub(1) / 3, 0)
    }

    pub fn with_faults(size: usize, max_faulty: usize, max_offline: usize) -> Self {
        let slow = size.saturating_sub(max_faulty + max_offline);
        let fast = size.saturating_sub(max_offline);

        Self {
            size,
            max_faulty,
            max_offline,
            notarization: slow,
            fast,
            finalization: slow,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCommitteeSet {
    committees: Vec<RaiCommittee>,
}

impl RaiCommitteeSet {
    pub fn new(committees: impl IntoIterator<Item = RaiCommittee>) -> Self {
        let mut result = Vec::new();

        for committee in committees {
            if !result
                .iter()
                .any(|existing: &RaiCommittee| existing.has_same_members_as(&committee))
            {
                result.push(committee);
            }
        }

        Self { committees: result }
    }

    pub fn single(committee: RaiCommittee) -> Self {
        Self::new([committee])
    }

    pub fn iter(&self) -> impl Iterator<Item = &RaiCommittee> {
        self.committees.iter()
    }

    pub fn len(&self) -> usize {
        self.committees.len()
    }

    pub fn is_empty(&self) -> bool {
        self.committees.is_empty()
    }

    pub fn contains(&self, account: &PublicKey) -> bool {
        self.committees
            .iter()
            .any(|committee| committee.contains(account))
    }

    pub fn committee_indexes_for(&self, account: &PublicKey) -> Vec<usize> {
        self.committees
            .iter()
            .enumerate()
            .filter_map(|(index, committee)| committee.contains(account).then_some(index))
            .collect()
    }
}

#[derive(Clone, Default)]
pub struct RaiCommitteeDeriver {}

impl RaiCommitteeDeriver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn derive_genesis_committee(
        &self,
        balances: impl IntoIterator<Item = (PublicKey, Amount)>,
    ) -> RaiCommittee {
        self.derive_committee(balances)
    }

    pub fn derive_closed_epoch_committee(
        &self,
        _epoch: RaiEpoch,
        balances: impl IntoIterator<Item = (PublicKey, Amount)>,
    ) -> RaiCommittee {
        self.derive_committee(balances)
    }

    pub fn derive_committee(
        &self,
        balances: impl IntoIterator<Item = (PublicKey, Amount)>,
    ) -> RaiCommittee {
        let mut candidates = Vec::new();
        let mut total_weight = Amount::ZERO;

        for (account, balance) in balances {
            if balance.is_zero() {
                continue;
            }

            total_weight = total_weight
                .checked_add(balance)
                .expect("committee balance should never overflow");
            candidates.push(RaiCommitteeMember { account, balance });
        }

        let minimum_principal_balance =
            Amount::raw(total_weight.number() / RAI_PRINCIPAL_WEIGHT_DIVISOR);
        let mut members: Vec<_> = candidates
            .into_iter()
            .filter(|candidate| candidate.balance >= minimum_principal_balance)
            .collect();

        members.sort_by_key(|member| member.account);

        RaiCommittee {
            thresholds: RaiCommitteeThresholds::for_size(members.len()),
            members,
        }
    }
}

pub struct RepWeightRaiCommitteeProvider {
    rep_weights: Arc<RepWeightCache>,
    deriver: RaiCommitteeDeriver,
}

impl RepWeightRaiCommitteeProvider {
    pub fn new(rep_weights: Arc<RepWeightCache>) -> Self {
        Self {
            rep_weights,
            deriver: RaiCommitteeDeriver::new(),
        }
    }

    fn derive_from_current_weights(&self) -> RaiCommittee {
        let weights = self.rep_weights.read();
        self.deriver.derive_committee(
            weights
                .iter()
                .map(|(account, balance)| (*account, *balance)),
        )
    }
}

impl RaiCommitteeProvider for RepWeightRaiCommitteeProvider {
    fn genesis_committee(&self) -> RaiCommittee {
        self.derive_from_current_weights()
    }

    fn committee_for_closed_epoch(&self, _epoch: RaiEpoch) -> Option<RaiCommittee> {
        Some(self.derive_from_current_weights())
    }
}

fn election_epoch(election_id: &RaiElectionId) -> RaiEpoch {
    match election_id {
        RaiElectionId::Slot { epoch, .. } | RaiElectionId::Close { epoch, .. } => *epoch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_genesis_committee_from_genesis_balance() {
        let genesis = PublicKey::from(1);
        let committee =
            RaiCommitteeDeriver::new().derive_genesis_committee([(genesis, Amount::MAX)]);

        assert_eq!(committee.members().len(), 1);
        assert_eq!(committee.members()[0].account, genesis);
        assert_eq!(committee.members()[0].balance, Amount::MAX);
        assert_eq!(committee.thresholds().finalization, 1);
    }

    #[test]
    fn derives_principal_members_from_balances() {
        let large = PublicKey::from(1);
        let small = PublicKey::from(2);

        let committee = RaiCommitteeDeriver::new().derive_closed_epoch_committee(
            7,
            [
                (small, Amount::raw(1)),
                (large, Amount::raw(9999)),
                (PublicKey::from(3), Amount::ZERO),
            ],
        );

        assert_eq!(
            committee
                .members()
                .iter()
                .map(|member| member.account)
                .collect::<Vec<_>>(),
            vec![large]
        );
    }

    #[test]
    fn orders_members_deterministically_by_account() {
        let committee = RaiCommitteeDeriver::new().derive_committee([
            (PublicKey::from(3), Amount::raw(100)),
            (PublicKey::from(1), Amount::raw(100)),
            (PublicKey::from(2), Amount::raw(100)),
        ]);

        assert_eq!(
            committee
                .members()
                .iter()
                .map(|member| member.account)
                .collect::<Vec<_>>(),
            vec![PublicKey::from(1), PublicKey::from(2), PublicKey::from(3)]
        );
    }

    #[test]
    fn deduplicates_committees_with_the_same_members() {
        let first =
            RaiCommitteeDeriver::new().derive_committee([(PublicKey::from(1), Amount::raw(1))]);
        let second =
            RaiCommitteeDeriver::new().derive_committee([(PublicKey::from(1), Amount::raw(2))]);
        let committees = RaiCommitteeSet::new([first, second]);

        assert_eq!(committees.len(), 1);
    }
}
