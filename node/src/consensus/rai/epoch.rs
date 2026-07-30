use std::{collections::BTreeMap, sync::Arc};

use rsnano_ledger::{RepWeightCache, RepWeights};
use rsnano_types::{BlockHash, RaiEpoch};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RaiEpochPhase {
    #[default]
    Open,
    ClosingCut,
    Draining,
    ClosingRecord,
    Closed,
}

/// Owns the immutable representative-weight views used by RAI elections.
///
/// The live representative cache is deliberately not retained. A caller must
/// explicitly record a snapshot, which makes it impossible for later cache
/// updates to alter an already recorded committee.
pub struct RaiEpochManager {
    current_epoch: RaiEpoch,
    phase: RaiEpochPhase,
    genesis_committee: Arc<RepWeights>,
    committees: BTreeMap<RaiEpoch, Arc<RepWeights>>,
    close_hashes: BTreeMap<RaiEpoch, BlockHash>,
}

impl RaiEpochManager {
    pub fn new(genesis_committee: Arc<RepWeights>) -> Self {
        Self {
            current_epoch: RaiEpoch::ZERO,
            phase: RaiEpochPhase::Open,
            genesis_committee,
            committees: BTreeMap::new(),
            close_hashes: BTreeMap::new(),
        }
    }

    /// Freezes the currently visible weights for `epoch`.
    pub fn snapshot_committee(
        &mut self,
        epoch: RaiEpoch,
        live_weights: &RepWeightCache,
    ) -> Arc<RepWeights> {
        self.committees
            .entry(epoch)
            .or_insert_with(|| Arc::new(live_weights.read().clone()))
            .clone()
    }

    /// Installs an already frozen snapshot, for example while restoring state.
    pub fn insert_committee(
        &mut self,
        epoch: RaiEpoch,
        committee: Arc<RepWeights>,
    ) -> Option<Arc<RepWeights>> {
        self.committees.insert(epoch, committee)
    }

    pub fn record_close_hash(
        &mut self,
        epoch: RaiEpoch,
        close_hash: BlockHash,
    ) -> Option<BlockHash> {
        self.close_hashes.insert(epoch, close_hash)
    }

    pub fn current_epoch(&self) -> RaiEpoch {
        self.current_epoch
    }

    pub fn phase(&self) -> RaiEpochPhase {
        self.phase
    }

    pub fn set_phase(&mut self, phase: RaiEpochPhase) {
        self.phase = phase;
    }

    pub fn open_epoch(&mut self, epoch: RaiEpoch) {
        self.current_epoch = epoch;
        self.phase = RaiEpochPhase::Open;
    }

    /// Returns genesis for a negative epoch and requires recorded state for a
    /// non-negative epoch.
    pub fn committee_at(&self, epoch: i64) -> Option<Arc<RepWeights>> {
        if epoch < 0 {
            Some(self.genesis_committee.clone())
        } else {
            self.committees.get(&RaiEpoch::new(epoch as u64)).cloned()
        }
    }

    /// Committees eligible to vote on slots in `epoch` (`e-3` and `e-2`).
    /// Equal snapshots are returned once.
    pub fn slot_committees(&self, epoch: RaiEpoch) -> Option<Vec<Arc<RepWeights>>> {
        let epoch = i64::try_from(epoch.number()).ok()?;
        let first = self.committee_at(epoch.checked_sub(3)?)?;
        let second = self.committee_at(epoch.checked_sub(2)?)?;

        if Arc::ptr_eq(&first, &second) || first == second {
            Some(vec![first])
        } else {
            Some(vec![first, second])
        }
    }

    /// Committee eligible to vote on reports and the close for `epoch`.
    pub fn close_committee(&self, epoch: RaiEpoch) -> Option<Arc<RepWeights>> {
        let epoch = i64::try_from(epoch.number()).ok()?;
        self.committee_at(epoch.checked_sub(2)?)
    }

    /// The certified close which governs `epoch`.
    pub fn governing_hash(&self, epoch: RaiEpoch) -> Option<BlockHash> {
        let previous = epoch.number().checked_sub(1)?;
        self.close_hashes.get(&RaiEpoch::new(previous)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsnano_types::{Amount, PublicKey};

    fn weights(rep: u64, amount: u128) -> Arc<RepWeights> {
        Arc::new(RepWeights::from([(
            PublicKey::from(rep),
            Amount::raw(amount),
        )]))
    }

    #[test]
    fn negative_epochs_use_genesis() {
        let genesis = weights(1, 100);
        let manager = RaiEpochManager::new(genesis.clone());

        assert!(Arc::ptr_eq(&manager.committee_at(-1).unwrap(), &genesis));
        assert!(Arc::ptr_eq(&manager.committee_at(-100).unwrap(), &genesis));
    }

    #[test]
    fn early_epochs_collapse_duplicate_genesis_committees() {
        let manager = RaiEpochManager::new(weights(1, 100));

        assert_eq!(manager.slot_committees(0.into()).unwrap().len(), 1);
        assert_eq!(manager.slot_committees(1.into()).unwrap().len(), 1);
    }

    #[test]
    fn later_slots_select_both_historical_snapshots() {
        let first = weights(1, 100);
        let second = weights(2, 200);
        let mut manager = RaiEpochManager::new(weights(9, 900));
        manager.insert_committee(1.into(), first.clone());
        manager.insert_committee(2.into(), second.clone());

        let selected = manager.slot_committees(4.into()).unwrap();
        assert_eq!(selected, vec![first, second]);
    }

    #[test]
    fn close_selects_epoch_minus_two() {
        let expected = weights(1, 100);
        let mut manager = RaiEpochManager::new(weights(9, 900));
        manager.insert_committee(2.into(), expected.clone());

        assert!(Arc::ptr_eq(
            &manager.close_committee(4.into()).unwrap(),
            &expected
        ));
    }

    #[test]
    fn recorded_snapshot_is_not_changed_with_live_weights() {
        let live = RepWeightCache::default();
        let first = PublicKey::from(1);
        let second = PublicKey::from(2);
        live.put(first, Amount::raw(100));
        let mut manager = RaiEpochManager::new(weights(9, 900));

        let frozen = manager.snapshot_committee(0.into(), &live);
        live.put(first, Amount::ZERO);
        live.put(second, Amount::raw(200));

        assert_eq!(frozen.weight(&first), Amount::raw(100));
        assert_eq!(frozen.weight(&second), Amount::ZERO);
        assert_eq!(manager.committee_at(0).unwrap(), frozen);
    }

    #[test]
    fn missing_history_prevents_committee_selection_for_vote_validation() {
        let manager = RaiEpochManager::new(weights(1, 100));

        assert!(manager.slot_committees(3.into()).is_none());
        assert!(manager.close_committee(2.into()).is_none());
    }

    #[test]
    fn governing_hash_is_the_previous_epoch_close() {
        let mut manager = RaiEpochManager::new(weights(1, 100));
        let close = BlockHash::from(42);
        manager.record_close_hash(2.into(), close);

        assert_eq!(manager.governing_hash(3.into()), Some(close));
        assert_eq!(manager.governing_hash(2.into()), None);
        assert_eq!(manager.governing_hash(0.into()), None);
    }
}
