use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use rsnano_ledger::RepWeightCache;
use rsnano_types::{Amount, PublicKey, RaiElectionId, RaiEpoch, RaiVoteScope};

pub const RAI_PRINCIPAL_WEIGHT_DIVISOR: u128 = 1000;

/// Provides RAI committees derived from balance snapshots fixed by epoch close.
pub trait RaiCommitteeProvider: Send + Sync {
    fn genesis_committee(&self) -> RaiCommittee;
    fn committee_for_closed_epoch(&self, epoch: RaiEpoch) -> Option<RaiCommittee>;
    fn closed_epoch_rep_weight_snapshot(&self, _epoch: RaiEpoch) -> Option<RaiRepWeightSnapshot> {
        None
    }

    fn snapshot_closed_epoch_committee(&self, epoch: RaiEpoch) -> RaiCommittee {
        self.committee_at(Some(epoch))
    }

    fn committee_at(&self, epoch: Option<RaiEpoch>) -> RaiCommittee {
        epoch
            .and_then(|epoch| self.committee_for_closed_epoch(epoch))
            .unwrap_or_else(|| self.genesis_committee())
    }

    fn try_committee_at(&self, epoch: Option<RaiEpoch>) -> Option<RaiCommittee> {
        match epoch {
            None => Some(self.genesis_committee()),
            Some(epoch) => self.committee_for_closed_epoch(epoch),
        }
    }

    fn committees_for(&self, election_id: &RaiElectionId) -> RaiCommitteeSet {
        let epoch = election_epoch(election_id);
        RaiCommitteeSet::new([
            self.committee_at(epoch.checked_sub(3)),
            self.committee_at(epoch.checked_sub(2)),
        ])
    }

    fn try_committees_for(&self, election_id: &RaiElectionId) -> Option<RaiCommitteeSet> {
        let epoch = election_epoch(election_id);
        Some(RaiCommitteeSet::new([
            self.try_committee_at(epoch.checked_sub(3))?,
            self.try_committee_at(epoch.checked_sub(2))?,
        ]))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCommittee {
    members: Vec<RaiCommitteeMember>,
    thresholds: RaiCommitteeThresholds,
}

impl RaiCommittee {
    pub fn from_snapshot(snapshot: RaiCommitteeSnapshot) -> Self {
        let mut members = snapshot.members;
        members.sort_by_key(|member| member.account);

        Self {
            members,
            thresholds: snapshot.thresholds,
        }
    }

    pub fn snapshot(&self) -> RaiCommitteeSnapshot {
        RaiCommitteeSnapshot {
            members: self.members.clone(),
            thresholds: self.thresholds,
        }
    }

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaiCommitteeSnapshot {
    pub members: Vec<RaiCommitteeMember>,
    pub thresholds: RaiCommitteeThresholds,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RaiRepWeightSnapshot {
    weights: Vec<RaiRepWeight>,
}

impl RaiRepWeightSnapshot {
    pub fn from_weights(weights: impl IntoIterator<Item = (PublicKey, Amount)>) -> Self {
        let mut weights: Vec<_> = weights
            .into_iter()
            .filter(|(_, weight)| !weight.is_zero())
            .map(|(representative, weight)| RaiRepWeight {
                representative,
                weight,
            })
            .collect();
        weights.sort_by_key(|entry| entry.representative);

        Self { weights }
    }

    pub fn weights(&self) -> &[RaiRepWeight] {
        &self.weights
    }

    fn balances(&self) -> impl Iterator<Item = (PublicKey, Amount)> + '_ {
        self.weights
            .iter()
            .map(|entry| (entry.representative, entry.weight))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RaiRepWeight {
    pub representative: PublicKey,
    pub weight: Amount,
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

    pub fn scoped_committee_indexes_for(
        &self,
        account: &PublicKey,
        scope: RaiVoteScope,
    ) -> Vec<usize> {
        match scope {
            RaiVoteScope::All => self.committee_indexes_for(account),
            RaiVoteScope::Committee(index) => {
                let index = index as usize;
                self.committees
                    .get(index)
                    .filter(|committee| committee.contains(account))
                    .map(|_| vec![index])
                    .unwrap_or_default()
            }
        }
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
    genesis_snapshot: RaiRepWeightSnapshot,
    closed_rep_weight_snapshots: RwLock<HashMap<RaiEpoch, RaiRepWeightSnapshot>>,
    closed_committees: RwLock<HashMap<RaiEpoch, RaiCommittee>>,
}

impl RepWeightRaiCommitteeProvider {
    pub fn new(rep_weights: Arc<RepWeightCache>) -> Self {
        Self::with_closed_epoch_snapshots(rep_weights, Vec::new(), Vec::new())
    }

    pub fn with_closed_epoch_snapshots(
        rep_weights: Arc<RepWeightCache>,
        closed_rep_weight_snapshots: impl IntoIterator<Item = (RaiEpoch, RaiRepWeightSnapshot)>,
        closed_committees: impl IntoIterator<Item = (RaiEpoch, RaiCommittee)>,
    ) -> Self {
        let genesis_snapshot = Self::snapshot_current_weights(&rep_weights);

        Self {
            rep_weights,
            deriver: RaiCommitteeDeriver::new(),
            genesis_snapshot,
            closed_rep_weight_snapshots: RwLock::new(
                closed_rep_weight_snapshots.into_iter().collect(),
            ),
            closed_committees: RwLock::new(closed_committees.into_iter().collect()),
        }
    }

    pub fn with_closed_committees(
        rep_weights: Arc<RepWeightCache>,
        closed_committees: impl IntoIterator<Item = (RaiEpoch, RaiCommittee)>,
    ) -> Self {
        Self::with_closed_epoch_snapshots(rep_weights, Vec::new(), closed_committees)
    }

    fn snapshot_current_weights(rep_weights: &RepWeightCache) -> RaiRepWeightSnapshot {
        let weights = rep_weights.read();
        RaiRepWeightSnapshot::from_weights(
            weights
                .iter()
                .map(|(representative, weight)| (*representative, *weight)),
        )
    }

    fn derive_genesis_from_snapshot(&self) -> RaiCommittee {
        self.deriver
            .derive_genesis_committee(self.genesis_snapshot.balances())
    }

    fn derive_closed_from_snapshot(
        &self,
        epoch: RaiEpoch,
        snapshot: &RaiRepWeightSnapshot,
    ) -> RaiCommittee {
        self.deriver
            .derive_closed_epoch_committee(epoch, snapshot.balances())
    }
}

impl RaiCommitteeProvider for RepWeightRaiCommitteeProvider {
    fn genesis_committee(&self) -> RaiCommittee {
        self.derive_genesis_from_snapshot()
    }

    fn committee_for_closed_epoch(&self, epoch: RaiEpoch) -> Option<RaiCommittee> {
        if let Some(snapshot) = self
            .closed_rep_weight_snapshots
            .read()
            .unwrap()
            .get(&epoch)
            .cloned()
        {
            let committee = self.derive_closed_from_snapshot(epoch, &snapshot);
            self.closed_committees
                .write()
                .unwrap()
                .entry(epoch)
                .or_insert_with(|| committee.clone());
            Some(committee)
        } else {
            self.closed_committees.read().unwrap().get(&epoch).cloned()
        }
    }

    fn closed_epoch_rep_weight_snapshot(&self, epoch: RaiEpoch) -> Option<RaiRepWeightSnapshot> {
        self.closed_rep_weight_snapshots
            .read()
            .unwrap()
            .get(&epoch)
            .cloned()
    }

    fn snapshot_closed_epoch_committee(&self, epoch: RaiEpoch) -> RaiCommittee {
        if let Some(committee) = self.committee_for_closed_epoch(epoch) {
            return committee;
        }

        let snapshot = {
            let mut closed_snapshots = self.closed_rep_weight_snapshots.write().unwrap();
            closed_snapshots
                .entry(epoch)
                .or_insert_with(|| Self::snapshot_current_weights(&self.rep_weights))
                .clone()
        };
        let committee = self.derive_closed_from_snapshot(epoch, &snapshot);
        self.closed_committees
            .write()
            .unwrap()
            .entry(epoch)
            .or_insert_with(|| committee.clone());
        committee
    }
}

fn election_epoch(election_id: &RaiElectionId) -> RaiEpoch {
    match election_id {
        RaiElectionId::Slot { epoch, .. }
        | RaiElectionId::CloseCut { epoch, .. }
        | RaiElectionId::CloseRecord { epoch, .. } => *epoch,
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

    #[test]
    fn provider_keeps_closed_epoch_committee_fixed_from_weight_snapshot() {
        let first = PublicKey::from(1);
        let second = PublicKey::from(2);
        let weights = Arc::new(RepWeightCache::default());
        weights.put(first, Amount::raw(100));
        let provider = RepWeightRaiCommitteeProvider::new(weights.clone());

        let closed_committee = provider.snapshot_closed_epoch_committee(0);
        weights.put(first, Amount::ZERO);
        weights.put(second, Amount::raw(100));

        let loaded_committee = provider.committee_for_closed_epoch(0).unwrap();
        assert_eq!(loaded_committee, closed_committee);
        assert!(loaded_committee.contains(&first));
        assert!(!loaded_committee.contains(&second));
    }

    #[test]
    fn provider_derives_closed_epoch_committee_from_loaded_weight_snapshot() {
        let historical = PublicKey::from(1);
        let current = PublicKey::from(2);
        let weights = Arc::new(RepWeightCache::default());
        weights.put(current, Amount::raw(100));
        let provider = RepWeightRaiCommitteeProvider::with_closed_epoch_snapshots(
            weights,
            [(
                4,
                RaiRepWeightSnapshot::from_weights([(historical, Amount::raw(100))]),
            )],
            Vec::new(),
        );

        let committee = provider.committee_for_closed_epoch(4).unwrap();

        assert!(committee.contains(&historical));
        assert!(!committee.contains(&current));
    }
}
